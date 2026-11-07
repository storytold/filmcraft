//! Saved EffectCraft snapshots: a resident renderer, four-frame prefetch and one-second audio chunks.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, SharedSource};
use filmcraft_project::project_link::ProjectLink;
use filmcraft_time::Tick;

use crate::project_links::{EffectCraft, ProjectAdapter};
use crate::{MediaPool, Services};

const VIDEO_CHUNK: i64 = 4;
static RENDERER: LazyLock<RenderGate> = LazyLock::new(RenderGate::default);

struct ActiveRenderer {
    revision: String,
    progress: Arc<filmcraft_export::Progress>,
    process: crate::effectcraft_resident::Resident,
}

#[derive(Default)]
struct RenderGate {
    resident: Mutex<Option<ActiveRenderer>>,
    foreground: AtomicUsize,
}

impl RenderGate {
    fn lock(&self, progress: &filmcraft_export::Progress) -> Result<MutexGuard<'_, Option<ActiveRenderer>>, String> {
        struct Waiting<'a>(&'a AtomicUsize, bool);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                if self.1 {
                    self.0.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
        let foreground = !filmcraft_media::cancel::background() && filmcraft_media::cancel::catch_up().is_none_or(|margin| margin.0 <= 0);
        if foreground {
            self.foreground.fetch_add(1, Ordering::Relaxed);
        }
        let _waiting = Waiting(&self.foreground, foreground);
        let started = web_time::Instant::now();
        loop {
            if filmcraft_media::cancel::cancelled() || progress.cancel.load(Ordering::Relaxed) {
                return Err("frame request cancelled".into());
            }
            // Playback already queues ahead; waiting thumbnails and prefetch yield to its playhead.
            if foreground || self.foreground.load(Ordering::Relaxed) == 0 {
                match self.resident.try_lock() {
                    Ok(guard) => return Ok(guard),
                    Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
                    Err(std::sync::TryLockError::WouldBlock) => {}
                }
            }
            if started.elapsed().as_secs() >= 600 {
                return Err("frame request queue timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

pub(crate) fn create(
    executable: PathBuf,
    services: Arc<dyn Services>,
    link: ProjectLink,
    budget: u64,
    progress: Arc<filmcraft_export::Progress>,
) -> Result<SharedSource, String> {
    let adapter = EffectCraft { executable };
    let object = adapter.objects(&*services, &link.project_path)?.into_iter().find(|o| o.id == link.object).ok_or("composition was removed")?;
    let generator = filmcraft_media::generators::GeneratorSource::new(
        filmcraft_media::Generator::TransparentVideo,
        object.width,
        object.height,
        object.rate,
        object.duration,
    );
    let mut info = generator.info().clone();
    info.name = object.name;
    info.kind = filmcraft_media::MediaKind::Movie;
    info.container = "EffectCraft on demand".into();
    info.audio_streams = vec![filmcraft_media::AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: "PCM".into(), bits_per_sample: Some(24) }];
    if let Some(video) = info.video.as_mut() {
        video.has_alpha = true;
        video.codec = "ProRes 4444 on demand".into();
    }
    Ok(Arc::new(Source { adapter, services, link, info, budget, progress }))
}

struct Source {
    adapter: EffectCraft,
    services: Arc<dyn Services>,
    link: ProjectLink,
    info: MediaInfo,
    budget: u64,
    progress: Arc<filmcraft_export::Progress>,
}

impl Source {
    fn fresh(&self) -> Result<(), String> {
        if filmcraft_media::cancel::cancelled() || self.progress.cancel.load(Ordering::Relaxed) {
            return Err("frame request cancelled".into());
        }
        if self.adapter.revision(&*self.services, &self.link)? != self.link.revision {
            return Err("composition changed; wait for link refresh".into());
        }
        Ok(())
    }

    fn chunk(&self, index: i64, audio: bool) -> Result<SharedSource, String> {
        if filmcraft_media::cancel::cancelled() {
            return Err("frame request cancelled".into());
        }
        let dir = crate::project_links::cache_dir();
        crate::project_links::private_dir(&dir)?;
        let extension = if audio { "wav" } else { "mov" };
        let kind = if audio { "a" } else { "v4" };
        let path = dir.join(format!("{}-{kind}-{index}.{extension}", self.link.revision));
        let pool = MediaPool::default();
        if path.is_file() {
            return pool.open_file(&path.to_string_lossy(), &*self.services).map_err(|e| e.to_string());
        }
        let mut resident = RENDERER.lock(&self.progress)?;
        if path.is_file() {
            return pool.open_file(&path.to_string_lossy(), &*self.services).map_err(|e| e.to_string());
        }
        self.fresh()?;
        let max_bytes = crate::linked_cache::allowance(&dir, self.budget)?;
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let part =
            dir.join(format!("{}-{kind}-{index}-{}-{}.part.{extension}", self.link.revision, std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        let rate = self.info.frame_rate();
        let start = if audio { index as f64 } else { rate.tick_of(index.saturating_mul(VIDEO_CHUNK)).seconds() };
        let end =
            if audio { start + 1.0 } else { rate.tick_of(index.saturating_add(1).saturating_mul(VIDEO_CHUNK)).seconds() }.min(self.info.duration.seconds());
        let mut params = serde_json::json!({"comp":self.link.object,"output":part,"start":start,"end":end});
        if audio {
            params["format"] = serde_json::json!("wav");
            params["audio"] = serde_json::json!("on");
        } else {
            params["format"] = serde_json::json!("prores");
            params["proresProfile"] = serde_json::json!("4444");
            params["channels"] = serde_json::json!("rgba");
            params["audio"] = serde_json::json!("off");
        }
        *self.progress.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = if audio {
            format!("Rendering audio second {index}")
        } else {
            format!("Rendering frames {}–{}", index * VIDEO_CHUNK, index * VIDEO_CHUNK + VIDEO_CHUNK - 1)
        };
        self.progress.finished.store(false, Ordering::Relaxed);
        let result = (|| {
            if resident
                .as_ref()
                .is_none_or(|active| active.revision != self.link.revision || !Arc::ptr_eq(&active.progress, &self.progress) || !active.process.alive())
            {
                // One resident renderer bounds memory even when a document has many linked comps.
                resident.take();
                let process =
                    crate::effectcraft_resident::Resident::open(&self.adapter.executable.to_string_lossy(), &self.link.project_path, self.progress.clone())?;
                *resident = Some(ActiveRenderer { revision: self.link.revision.clone(), progress: self.progress.clone(), process });
                self.fresh()?;
            }
            resident.as_mut().ok_or("EffectCraft session unavailable")?.process.render(params, &part, max_bytes, &self.progress)?;
            self.fresh()?;
            let size = std::fs::metadata(&part).map_err(|e| e.to_string())?.len();
            if size == 0 || size > max_bytes || crate::linked_cache::bytes(&dir)? > self.budget {
                return Err("on-demand cache quota exceeded".into());
            }
            let source = pool.open_file(&part.to_string_lossy(), &*self.services).map_err(|e| e.to_string())?;
            if (audio && !source.info().has_audio()) || (!audio && !source.info().has_video()) {
                return Err("renderer returned an invalid stream".into());
            }
            drop(source);
            // FlushFileBuffers requires a writable handle on Windows.
            std::fs::OpenOptions::new().write(true).open(&part).and_then(|file| file.sync_all()).map_err(|e| format!("sync on-demand cache: {e}"))?;
            // Same-directory hard links publish exclusively without another full-size copy.
            std::fs::hard_link(&part, &path)
                .or_else(|error| if error.kind() == std::io::ErrorKind::AlreadyExists { Ok(()) } else { Err(error) })
                .map_err(|e| format!("publish on-demand cache: {e}"))?;
            pool.open_file(&path.to_string_lossy(), &*self.services).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            resident.take();
        }
        self.progress.finished.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&part);
        result
    }

    fn error(&self, error: String) -> MediaError {
        if filmcraft_media::cancel::cancelled() || self.progress.cancel.load(Ordering::Relaxed) { MediaError::Cancelled } else { MediaError::Decode(error) }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
        if let Ok(mut active) = RENDERER.resident.try_lock()
            && active.as_ref().is_some_and(|renderer| Arc::ptr_eq(&renderer.progress, &self.progress))
        {
            active.take();
        }
    }
}

impl MediaSource for Source {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, request: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let last = self.info.frame_rate().frame_at(Tick(self.info.duration.0.saturating_sub(1))).max(0);
        let index = self.info.frame_rate().frame_at(request.time).clamp(0, last);
        let chunk = index / VIDEO_CHUNK;
        let relative = self.info.frame_rate().tick_of(index - chunk * VIDEO_CHUNK);
        self.chunk(chunk, false).map_err(|error| self.error(error))?.video_frame(FrameRequest::full(relative))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        filmcraft_media::bounded_audio_dimensions(sample_rate, 2)?;
        filmcraft_media::validate_audio_buffer_shape(2, frames)?;
        if frames > sample_rate as usize * 10 {
            return Err(MediaError::Decode("on-demand audio requests are limited to ten seconds".into()));
        }
        let mut output = AudioBuffer::silence(sample_rate, 2, frames);
        let duration = (self.info.duration.seconds() * f64::from(sample_rate)).ceil() as i64;
        let mut offset = 0;
        while offset < frames {
            let position = start.saturating_add(offset as i64);
            if position < 0 {
                offset = frames.min(offset.saturating_add(position.saturating_neg() as usize));
                continue;
            }
            if position >= duration {
                break;
            }
            let second = position / i64::from(sample_rate);
            let within = position % i64::from(sample_rate);
            let count = (frames - offset).min((i64::from(sample_rate) - within) as usize).min((duration - position) as usize);
            let buffer = self.chunk(second, true).map_err(|error| self.error(error))?.audio(within, count, sample_rate)?;
            for (channel, target) in output.channels.iter_mut().enumerate() {
                if let (Some(target), Some(source)) = (target.get_mut(offset..offset + count), buffer.channels.get(channel).and_then(|c| c.get(..count))) {
                    target.copy_from_slice(source);
                } else {
                    return Err(MediaError::Decode("invalid on-demand audio buffer".into()));
                }
            }
            offset += count;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_a_queued_source_does_not_wait_for_another_sources_render() {
        let gate = Arc::new(RenderGate::default());
        let held = gate.lock(&Default::default()).unwrap();
        let progress = Arc::new(filmcraft_export::Progress::default());
        let waiting = gate.clone();
        let cancelled = progress.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let error = waiting.lock(&cancelled).err();
            sender.send(error).unwrap();
        });
        let start = web_time::Instant::now();
        while gate.foreground.load(Ordering::Relaxed) == 0 {
            assert!(start.elapsed().as_secs() < 3);
            std::thread::yield_now();
        }
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(receiver.recv_timeout(std::time::Duration::from_secs(3)).unwrap().unwrap().contains("cancelled"));
        assert_eq!(gate.foreground.load(Ordering::Relaxed), 0);
        drop(held);
        thread.join().unwrap();
    }

    #[test]
    fn waiting_playhead_overtakes_background_work() {
        let gate = Arc::new(RenderGate::default());
        let held = gate.lock(&Default::default()).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let background = gate.clone();
        let background_sender = sender.clone();
        let bg = std::thread::spawn(move || {
            filmcraft_media::cancel::with_background(true, || {
                let _guard = background.lock(&Default::default()).unwrap();
                background_sender.send("background").unwrap();
            })
        });
        let foreground = gate.clone();
        let fg = std::thread::spawn(move || {
            let _guard = foreground.lock(&Default::default()).unwrap();
            sender.send("playhead").unwrap();
        });
        let start = web_time::Instant::now();
        while gate.foreground.load(Ordering::Relaxed) == 0 {
            assert!(start.elapsed().as_secs() < 3);
            std::thread::yield_now();
        }
        drop(held);
        assert_eq!(receiver.recv_timeout(std::time::Duration::from_secs(3)).unwrap(), "playhead");
        assert_eq!(receiver.recv_timeout(std::time::Duration::from_secs(3)).unwrap(), "background");
        fg.join().unwrap();
        bg.join().unwrap();
        assert_eq!(gate.foreground.load(Ordering::Relaxed), 0);
    }
}
