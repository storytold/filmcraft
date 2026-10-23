//! One identified picture component and stable, independent audio channels.
use crate::media_pool::SlateSource;
use crate::{Services, component_audio};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource};
use filmcraft_project::{AudioComponent, PictureComponent};
use filmcraft_render::offline::OfflineReason;
use filmcraft_time::Tick;
use std::sync::Arc;

pub fn open_picture(c: &PictureComponent, services: &dyn Services) -> Result<SharedSource, MediaError> {
    if c.track == 0 || c.length <= Tick::ZERO || c.length > Tick::MAX {
        return Err(MediaError::Unsupported("invalid picture component timing or track".into()));
    }
    component_audio::open_mxf(c.path.as_deref(), c.file_package, filmcraft_mxf::TrackKind::Picture, c.source_start, c.length, services)
}

/// Playback and status must agree on whether the saved picture binding is usable.
/// Relink uses open_picture directly so it can validate and hydrate a pending format.
pub(super) fn open_bound_picture(c: &PictureComponent, info: &MediaInfo, services: &dyn Services) -> Result<SharedSource, MediaError> {
    if c.format_pending {
        return Err(MediaError::Unsupported("picture format pending identity-checked relink".into()));
    }
    let source = open_picture(c, services)?;
    if source.info().video != info.video {
        return Err(MediaError::Unsupported("picture format changed; rebind the verified source".into()));
    }
    Ok(source)
}

struct ComponentMedia {
    info: MediaInfo,
    picture: PictureComponent,
    video: SharedSource,
    audio: Option<SharedSource>,
    picture_available: bool,
}
impl MediaSource for ComponentMedia {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, mut req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        if req.time < Tick::ZERO || req.time >= self.picture.length {
            return Err(MediaError::Decode("picture request outside component range".into()));
        }
        let local = req.time;
        if self.picture_available {
            req.time = Tick(req.time.0.checked_add(self.picture.source_start.0).ok_or_else(|| MediaError::Decode("picture time overflow".into()))?);
        }
        let frame = self.video.video_frame(req)?;
        // Plane buffers remain shared; expose presentation time in this logical source's domain.
        let mut frame = (*frame).clone();
        frame.pts = local;
        Ok(Arc::new(frame))
    }
    fn audio(&self, start: i64, frames: usize, rate: u32) -> Result<AudioBuffer, MediaError> {
        self.audio.as_ref().ok_or(MediaError::NoStream("audio"))?.audio(start, frames, rate)
    }
}

pub fn open(
    info: MediaInfo,
    channels: &[AudioComponent],
    picture: Option<&PictureComponent>,
    services: &dyn Services,
) -> Result<(SharedSource, Vec<String>), MediaError> {
    let Some(picture) = picture else { return component_audio::open(info, channels, services) };
    if info.video.is_none() || info.duration <= Tick::ZERO || info.duration > Tick::MAX || picture.length != info.duration || channels.len() > 64 {
        return Err(MediaError::Unsupported("invalid picture/audio component layout".into()));
    }
    if info.video.as_ref().is_some_and(|v| {
        v.width == 0
            || v.height == 0
            || v.width > 16384
            || v.height > 16384
            || u64::from(v.width) * u64::from(v.height) > 64 * 1024 * 1024
            || v.frame_rate.num <= 0
            || v.frame_rate.den <= 0
    }) {
        return Err(MediaError::Unsupported("invalid component picture geometry or rate".into()));
    }
    let mut failures = Vec::new();
    let opened = open_bound_picture(picture, &info, services);
    let (video, picture_available) = match opened {
        Ok(source) => (source, true),
        Err(e) => {
            failures.push(format!("V{}: {e}", picture.track));
            (Arc::new(SlateSource::new(info.clone(), &info.name, OfflineReason::Unreadable)) as SharedSource, false)
        }
    };
    let audio = if channels.is_empty() {
        if !info.audio_streams.is_empty() {
            return Err(MediaError::Unsupported("audio metadata without components".into()));
        }
        None
    } else {
        let mut audio_info = info.clone();
        audio_info.video = None;
        audio_info.kind = MediaKind::AudioOnly;
        let (source, issues) = component_audio::open(audio_info, channels, services)?;
        failures.extend(issues);
        Some(source)
    };
    Ok((Arc::new(ComponentMedia { info, picture: picture.clone(), video, audio, picture_available }), failures))
}
