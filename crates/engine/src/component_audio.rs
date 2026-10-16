//! Logical audio channels backed by independently identified OP-Atom files.
use crate::Services;
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, SharedSource};
use filmcraft_project::AudioComponent;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use std::sync::Arc;

fn bad(message: &str) -> MediaError {
    MediaError::Unsupported(message.into())
}

/// Open exactly the expected essence package. A matching filename is insufficient.
pub fn open_component(c: &AudioComponent, services: &dyn Services) -> Result<SharedSource, MediaError> {
    let source = open_mxf(c.path.as_deref(), c.file_package, filmcraft_mxf::TrackKind::Sound, c.source_start, c.length, services)?;
    let stream = source.info().audio_streams.get(c.stream).ok_or_else(|| bad("component audio stream is missing"))?;
    if c.channel >= stream.channels as usize {
        return Err(bad("component audio channel is missing"));
    }
    Ok(source)
}

pub(super) fn open_mxf(
    path: Option<&str>,
    identity: [u8; 32],
    kind: filmcraft_mxf::TrackKind,
    start: Tick,
    length: Tick,
    services: &dyn Services,
) -> Result<SharedSource, MediaError> {
    Ok(Arc::new(inspect_mxf(path, identity, kind, start, length, services)?))
}
/// Typed metadata access for import corroboration; uses the same identity/profile/range checks.
pub(super) fn inspect_mxf(
    path: Option<&str>,
    identity: [u8; 32],
    kind: filmcraft_mxf::TrackKind,
    start: Tick,
    length: Tick,
    services: &dyn Services,
) -> Result<filmcraft_codecs::mxf::MxfSource, MediaError> {
    if identity == [0; 32] {
        return Err(bad("component has no file-package identity"));
    }
    let path = path.ok_or_else(|| MediaError::Offline("component has no located file".into()))?;
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        return Err(bad("invalid component path"));
    }
    let reader =
        services.reader(path).ok_or_else(|| bad("component media requires random-access file services"))?.map_err(|e| MediaError::Io(e.to_string()))?;
    let source = filmcraft_codecs::mxf::MxfSource::open_reader(path, reader).map_err(|e| MediaError::Decode(e.to_string()))?;
    let tracks = source.essence_tracks();
    if source.info().container != "MXF OP-Atom" || tracks.len() != 1 || tracks.first().is_none_or(|t| t.file_package_uid != identity || t.kind != kind) {
        return Err(bad("MXF component identity or single-track profile does not match"));
    }
    if start < Tick::ZERO || length < Tick::ZERO || start.0.checked_add(length.0).is_none_or(|n| n > source.info().duration.0) {
        return Err(bad("component source range exceeds decoded media"));
    }
    Ok(source)
}

pub struct ComponentAudio {
    info: MediaInfo,
    parts: Vec<(AudioComponent, Option<SharedSource>)>,
}
impl ComponentAudio {
    pub fn new(info: MediaInfo, parts: Vec<(AudioComponent, Option<SharedSource>)>) -> Result<Self, MediaError> {
        if parts.is_empty()
            || parts.len() > 64
            || info.duration < Tick::ZERO
            || info.duration > Tick::MAX
            || info.video.is_some()
            || info.audio_streams.len() != 1
            || info.audio().is_none_or(|a| a.channels as usize != parts.len() || a.sample_rate == 0)
        {
            return Err(bad("invalid composite audio layout"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (c, source) in &parts {
            if c.track == 0
                || !seen.insert(c.track)
                || c.source_start < Tick::ZERO
                || c.source_start > Tick::MAX
                || c.length < Tick::ZERO
                || c.length > info.duration
            {
                return Err(bad("invalid component track or timing"));
            }
            if let Some(source) = source
                && source.info().audio_streams.get(c.stream).is_none_or(|a| c.channel >= a.channels as usize)
            {
                return Err(bad("component channel out of range"));
            }
        }
        Ok(Self { info, parts })
    }
}
impl MediaSource for ComponentAudio {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        Err(MediaError::NoStream("video"))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        if sample_rate == 0 || sample_rate > 768000 || frames.checked_mul(self.parts.len()).is_none_or(|n| n > 8 * 1024 * 1024) {
            return Err(bad("composite audio request exceeds limits"));
        }
        let mut out = AudioBuffer::silence(sample_rate, self.parts.len(), frames);
        let request_end = i128::from(start).checked_add(frames as i128).ok_or_else(|| bad("audio range overflow"))?;
        for ((component, source), channel) in self.parts.iter().zip(&mut out.channels) {
            let Some(source) = source else { continue };
            let end = (i128::from(component.length.0) * i128::from(sample_rate) + i128::from(TICKS_PER_SECOND) - 1) / i128::from(TICKS_PER_SECOND);
            let begin = i128::from(start).max(0);
            let finish = request_end.min(end);
            if finish <= begin {
                continue;
            }
            let offset = i128::from(component.source_start.0) * i128::from(sample_rate) / i128::from(TICKS_PER_SECOND);
            let source_start = i64::try_from(begin + offset).map_err(|_| bad("source sample index overflow"))?;
            let count = usize::try_from(finish - begin).map_err(|_| bad("audio count overflow"))?;
            let dest = usize::try_from(begin - i128::from(start)).map_err(|_| bad("audio destination overflow"))?;
            let decoded = source.audio_stream(component.stream, source_start, count, sample_rate)?;
            let samples = decoded.channels.get(component.channel).ok_or_else(|| bad("decoder omitted a component channel"))?;
            if decoded.sample_rate != sample_rate || samples.len() != count {
                return Err(bad("decoder returned an inconsistent audio buffer"));
            }
            let target = dest.checked_add(count).and_then(|end| channel.get_mut(dest..end)).ok_or_else(|| bad("audio destination exceeds request"))?;
            target.copy_from_slice(samples);
        }
        Ok(out)
    }
}

/// Keep failed components silent in their original slots and expose each failure to the caller.
pub fn open(info: MediaInfo, channels: &[AudioComponent], services: &dyn Services) -> Result<(SharedSource, Vec<String>), MediaError> {
    if channels.len() > 64 {
        return Err(bad("too many audio components"));
    }
    let mut failures = Vec::new();
    let parts = channels
        .iter()
        .map(|c| {
            let source = match open_component(c, services) {
                Ok(source) => Some(source),
                Err(e) => {
                    failures.push(format!("A{}: {e}", c.track));
                    None
                }
            };
            (c.clone(), source)
        })
        .collect();
    Ok((Arc::new(ComponentAudio::new(info, parts)?), failures))
}
