//! Bounded, asynchronous reference previews. No preview imports media into the project.
use std::{
    collections::HashMap,
    sync::{
        Arc,
        mpsc::{Receiver, TryRecvError},
    },
    time::Instant,
};

use crate::FilmcraftApp;
use egui::{ColorImage, Context, TextureHandle};
use filmcraft_frame::AudioBuffer;
use filmcraft_time::Tick;

const SAMPLE_RATE: u32 = 24_000;
const CACHE_LIMIT: usize = 24;

pub(super) enum Preview {
    Image(ColorImage),
    Audio { sound: Arc<AudioBuffer>, peaks: Vec<f32> },
}

struct Entry {
    receiver: Option<Receiver<Result<Preview, String>>>,
    preview: Option<Preview>,
    texture: Option<TextureHandle>,
    error: Option<String>,
    used: u64,
}

#[derive(Default)]
pub(super) struct Media {
    entries: HashMap<(String, bool), Entry>,
    clock: u64,
    pub playback: Option<Playback>,
}

pub(super) struct Playback {
    pub path: String,
    pub position: Tick,
    pub playing: bool,
    offset: Tick,
    started: Instant,
    last_frames: u64,
    last_progress: Instant,
}

impl Media {
    pub fn request(&mut self, app: &FilmcraftApp, path: &str, audio: bool) {
        self.clock = self.clock.saturating_add(1);
        let key = (path.to_string(), audio);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.used = self.clock;
            return;
        }
        if self.entries.len() >= CACHE_LIMIT {
            let playing = self.playback.as_ref().filter(|playback| playback.playing).map(|playback| playback.path.as_str());
            let old = self
                .entries
                .iter()
                .filter(|((path, audio), entry)| entry.receiver.is_none() && !(*audio && playing == Some(path.as_str())))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            if let Some(old) = old {
                self.entries.remove(&old);
            } else {
                return;
            }
        }
        let pool = app.session.media.clone();
        let services = app.session.services.clone();
        let source_path = path.to_string();
        let receiver = worker(move || {
            let limit = if audio || !source_path.to_ascii_lowercase().ends_with(".mp4") { 25 * 1024 * 1024 } else { 2 * 1024 * 1024 * 1024 };
            if services.file_size(&source_path).map_err(|e| e.to_string())? > limit {
                return Err("Media is too large to preview.".into());
            }
            let source = pool.probe_file(&source_path, &*services).map_err(|e| e.to_string())?;
            if audio {
                let duration = source.info().duration;
                if duration.0 <= 0 || duration > Tick::from_units(15, 1) {
                    return Err("Audio references must be at most 15 seconds.".into());
                }
                let channels = source.info().audio().ok_or("No audio stream")?.channels;
                if channels == 0 || channels > 2 {
                    return Err("Use mono or stereo audio references.".into());
                }
                let frames = usize::try_from(duration.to_units_floor(i64::from(SAMPLE_RATE))).map_err(|e| e.to_string())?;
                let mut sound = source.audio(0, frames.min(15 * SAMPLE_RATE as usize), SAMPLE_RATE).map_err(|e| e.to_string())?;
                if sound.sample_rate == 0 || sound.frames() == 0 || sound.channels.len() > 2 {
                    return Err("Audio decoder returned an invalid preview.".into());
                }
                for channel in &mut sound.channels {
                    channel.truncate(15 * SAMPLE_RATE as usize);
                    for sample in channel {
                        if !sample.is_finite() {
                            *sample = 0.0;
                        }
                    }
                }
                let peaks = waveform(&sound);
                Ok(Preview::Audio { sound: Arc::new(sound), peaks })
            } else {
                let video = source.info().video.as_ref().ok_or("No picture is available")?;
                if video.width == 0 || video.height == 0 || u64::from(video.width) * u64::from(video.height) > 25_000_000 {
                    return Err("Image dimensions exceed preview limits.".into());
                }
                let frame = source
                    .video_frame(filmcraft_media::FrameRequest { time: Tick::ZERO, scale: (320.0 / video.width.max(video.height) as f32).min(1.0) })
                    .map_err(|e| e.to_string())?;
                thumbnail(&frame).map(Preview::Image)
            }
        });
        self.entries.insert(key, Entry { receiver: Some(receiver), preview: None, texture: None, error: None, used: self.clock });
    }

    pub fn poll(&mut self, ctx: &Context) {
        for ((path, _), entry) in &mut self.entries {
            let next = entry.receiver.as_ref().map(Receiver::try_recv);
            let result = match next {
                Some(Ok(result)) => Some(result),
                Some(Err(TryRecvError::Disconnected)) => Some(Err("Preview worker stopped unexpectedly.".into())),
                _ => None,
            };
            if let Some(result) = result {
                entry.receiver = None;
                match result {
                    Ok(preview) => {
                        if let Preview::Image(image) = &preview {
                            entry.texture = Some(ctx.load_texture(format!("reactor-{path}"), image.clone(), egui::TextureOptions::LINEAR));
                        }
                        entry.preview = Some(preview);
                    }
                    Err(error) => entry.error = Some(error),
                }
            }
            if entry.receiver.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }

    pub fn texture(&self, path: &str) -> Option<&TextureHandle> {
        self.entries.get(&(path.into(), false))?.texture.as_ref()
    }
    pub fn error(&self, path: &str, audio: bool) -> Option<&str> {
        self.entries.get(&(path.into(), audio))?.error.as_deref()
    }
    pub fn audio(&self, path: &str) -> Option<(Arc<AudioBuffer>, &[f32])> {
        match self.entries.get(&(path.into(), true))?.preview.as_ref()? {
            Preview::Audio { sound, peaks } => Some((sound.clone(), peaks)),
            _ => None,
        }
    }

    pub fn advance(&mut self, app: &mut FilmcraftApp, ctx: &Context) {
        let Some(playback) = self.playback.as_mut().filter(|p| p.playing) else { return };
        if app.playback.playing || app.source_playback.clock.playing {
            playback.playing = false;
            return; // another monitor owns the output now
        }
        let path = playback.path.clone();
        let device = app.audio.as_ref().and_then(|a| a.played_frames().map(|frames| (frames, a.sample_rate())));
        if let Some((frames, _)) = device {
            if frames != playback.last_frames {
                playback.last_frames = frames;
                playback.last_progress = Instant::now();
            } else if playback.last_progress.elapsed() > std::time::Duration::from_secs(2) {
                playback.playing = false;
                if let Some(audio) = app.audio.as_mut() {
                    audio.stop();
                }
                app.ui.reactor.message = "Audio output stopped responding. Check Settings > Audio Hardware.".into();
                return;
            }
        }
        let elapsed = match device {
            Some((frames, rate)) if rate > 0 => Tick::from_units(i64::try_from(frames).unwrap_or(i64::MAX), i64::from(rate)),
            _ => Tick::from_seconds_f64(playback.started.elapsed().as_secs_f64()),
        };
        playback.position = Tick(playback.offset.0.saturating_add(elapsed.0));
        let position = playback.position;
        if let Some((sound, _)) = self.audio(&path) {
            let duration = Tick::from_units(sound.frames() as i64, i64::from(sound.sample_rate));
            if position >= duration {
                if let Some(playback) = self.playback.as_mut() {
                    playback.position = duration;
                    playback.playing = false;
                }
                if let Some(audio) = app.audio.as_mut() {
                    audio.stop();
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            }
        }
    }

    pub fn play(&mut self, app: &mut FilmcraftApp, path: &str, position: Tick) -> Result<(), String> {
        let (sound, _) = self.audio(path).ok_or("Audio preview is still loading or unavailable.")?;
        let duration = Tick::from_units(sound.frames() as i64, i64::from(sound.sample_rate));
        let position = position.max(Tick::ZERO).min(duration);
        let position = if position >= duration { Tick::ZERO } else { position };
        self.stop(app);
        app.stop();
        app.stop_source();
        let output = app.audio.as_mut().ok_or("Audio output is unavailable. Check Settings > Audio Hardware.")?;
        let rate = output.sample_rate();
        if rate == 0 || output.channels() == 0 {
            return Err("Invalid audio output configuration.".into());
        }
        output.stop();
        let offset = u64::try_from(position.to_units_floor(i64::from(sound.sample_rate))).unwrap_or(0);
        let mut cursor = 0u64;
        let fill = Box::new(move |buffer: &mut [f32], channels: usize| {
            for frame in buffer.chunks_mut(channels.max(1)) {
                let index = offset.saturating_add(cursor.saturating_mul(u64::from(sound.sample_rate)) / u64::from(rate));
                for (channel, value) in frame.iter_mut().enumerate() {
                    let source = sound.channels.get(channel).or_else(|| sound.channels.first());
                    *value = source.and_then(|data| usize::try_from(index).ok().and_then(|i| data.get(i))).copied().unwrap_or(0.0);
                }
                cursor = cursor.saturating_add(1);
            }
        });
        output.start(fill)?;
        self.playback = Some(Playback {
            path: path.into(),
            position,
            playing: true,
            offset: position,
            started: Instant::now(),
            last_frames: 0,
            last_progress: Instant::now(),
        });
        Ok(())
    }

    pub fn set_position(&mut self, path: &str, position: Tick) -> Result<(), String> {
        let (sound, _) = self.audio(path).ok_or("Audio preview is still loading or unavailable.")?;
        let position = position.max(Tick::ZERO).min(Tick::from_units(sound.frames() as i64, i64::from(sound.sample_rate)));
        self.playback = Some(Playback {
            path: path.into(),
            position,
            playing: false,
            offset: position,
            started: Instant::now(),
            last_frames: 0,
            last_progress: Instant::now(),
        });
        Ok(())
    }

    pub fn stop(&mut self, app: &mut FilmcraftApp) {
        if let Some(playback) = self.playback.as_mut() {
            if playback.playing
                && !app.playback.playing
                && !app.source_playback.clock.playing
                && let Some(audio) = app.audio.as_mut()
            {
                audio.stop();
            }
            playback.playing = false;
        }
    }
}

pub(super) fn worker<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Receiver<Result<T, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let sender = tx.clone();
        let started = std::thread::Builder::new().name("reactor-preview".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err("Media preview failed unexpectedly.".into()));
            let _ = sender.send(result);
        });
        if let Err(error) = started {
            let _ = tx.send(Err(format!("Could not start media preview: {error}")));
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = work;
        let _ = tx.send(Err("Reference preview is available in the desktop app.".into()));
    }
    rx
}

fn thumbnail(frame: &filmcraft_frame::VideoFrame) -> Result<ColorImage, String> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let pixels = width.checked_mul(height).filter(|&n| n > 0 && n <= 25_000_000).ok_or("Invalid preview dimensions")?;
    let rgba = frame.to_rgba8();
    if rgba.len() != pixels.saturating_mul(4) {
        return Err("Invalid preview pixels".into());
    }
    let scale = (320.0 / width.max(height) as f64).min(1.0);
    let (w, h) = ((width as f64 * scale).max(1.0) as usize, (height as f64 * scale).max(1.0) as usize);
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let offset = (y * height / h * width + x * width / w) * 4;
            let pixel = rgba.get(offset..offset + 4).ok_or("Invalid preview sample")?;
            out.push(egui::Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]));
        }
    }
    Ok(ColorImage::new([w, h], out))
}

fn waveform(sound: &AudioBuffer) -> Vec<f32> {
    let size = sound.frames().div_ceil(80).max(1);
    (0usize..80).map(|i| sound.channels.iter().flat_map(|c| c.iter().skip(i * size).take(size)).fold(0.0f32, |a, &v| a.max(v.abs()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnails_are_bounded_and_waveforms_follow_the_signal() {
        let frame = filmcraft_frame::VideoFrame::rgba8(640, 320, vec![200; 640 * 320 * 4]);
        assert_eq!(thumbnail(&frame).unwrap().size, [320, 160]);
        let sound = AudioBuffer { sample_rate: SAMPLE_RATE, channels: vec![vec![0.0; 800]] };
        assert!(waveform(&sound).iter().all(|&p| p == 0.0));
        let mut loud = sound;
        loud.channels[0][400] = -0.8;
        assert_eq!(waveform(&loud).iter().copied().fold(0.0f32, f32::max), 0.8);
    }
    #[test]
    fn audio_preview_seeks_and_yields_to_timeline_without_project_edits() {
        use std::{cell::RefCell, rc::Rc};
        struct Output(Rc<RefCell<Vec<f32>>>);
        impl crate::AudioOut for Output {
            fn start(&mut self, mut fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
                let mut data = vec![0.0; 8];
                fill(&mut data, 2);
                *self.0.borrow_mut() = data;
                Ok(48000)
            }
            fn stop(&mut self) {}
            fn sample_rate(&self) -> u32 {
                48000
            }
            fn played_frames(&self) -> Option<u64> {
                Some(0)
            }
        }
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let project = app.session.project.clone();
        let data = Rc::new(RefCell::new(Vec::new()));
        app.audio = Some(Box::new(Output(data.clone())));
        let mut media = Media::default();
        let mut samples = vec![0.0; 24000];
        samples[12000] = 0.5;
        media.entries.insert(
            ("voice.wav".into(), true),
            Entry {
                receiver: None,
                preview: Some(Preview::Audio { sound: Arc::new(AudioBuffer { sample_rate: 24000, channels: vec![samples] }), peaks: vec![] }),
                texture: None,
                error: None,
                used: 0,
            },
        );
        media.play(&mut app, "voice.wav", Tick::from_units(1, 2)).unwrap();
        assert_eq!(&data.borrow()[..4], &[0.5, 0.5, 0.5, 0.5]);
        assert!(media.playback.as_ref().unwrap().playing);
        media.stop(&mut app);
        assert!(!media.playback.as_ref().unwrap().playing);
        media.play(&mut app, "voice.wav", Tick::ZERO).unwrap();
        // Loading more thumbnails must retain the active sound so its end is still observed.
        for index in 1..CACHE_LIMIT {
            media.entries.insert((format!("thumbnail-{index}.png"), false), Entry { receiver: None, preview: None, texture: None, error: None, used: 1 });
        }
        media.request(&app, "/missing-reactor-cache-test.png", false);
        assert_eq!(media.entries.len(), CACHE_LIMIT);
        assert!(media.audio("voice.wav").is_some(), "cache eviction must preserve playing audio");
        app.playback.playing = true;
        media.advance(&mut app, &Context::default());
        assert!(!media.playback.as_ref().unwrap().playing);
        assert!(app.playback.playing);
        assert_eq!(app.session.project, project);
    }
}
