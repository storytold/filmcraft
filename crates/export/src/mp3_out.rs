//! MP3: the sequence mix alone, encoded by our MPEG Audio Layer III encoder (`filmcraft-mp3enc`)
//! at a constant bit rate into a plain `.mp3` stream of frames. MP3 carries 16-48 kHz, mono or
//! stereo: [`ExportSettings::mp3_stream`] maps the audio settings onto what it can carry. The
//! stream has no gapless (Xing / LAME) tag, so a player starts it with the encoder delay.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::TimeRange;

use crate::audio_out::AudioOut;
use crate::{ExportError, ExportSettings, Out, Progress, Result};

/// Encode the mix of `range` to `settings.path`; returns (bytes written, samples per channel).
pub(crate) fn export(
    project: &Arc<Project>,
    seq: ItemId,
    settings: &ExportSettings,
    range: TimeRange,
    sources: &dyn SourceProvider,
    progress: &Progress,
) -> Result<(u64, u64)> {
    let cancelled = || progress.cancel.load(Ordering::Relaxed);
    let seq_sr = project.sequence(seq).ok_or(ExportError::NoSequence)?.settings.sample_rate;
    let (rate, channels, kbps) = settings.mp3_stream(seq_sr);
    let mut s = settings.clone();
    s.audio.sample_rate = Some(rate);
    s.audio.channels = channels;
    let mut a = AudioOut::new(project.clone(), seq, &s, range)?;
    let cfg = filmcraft_mp3enc::Config::cbr(a.sr, a.channels, kbps);
    let mut enc = filmcraft_mp3enc::Encoder::new(cfg).map_err(|e| ExportError::Encode(e.to_string()))?;
    let frames = a.remaining();
    // one second per chunk: memory stays flat however long the range is
    let chunk = i64::from(a.sr.max(1));
    if !settings.part_of_batch {
        progress.total.store(frames.div_ceil(chunk as u64).max(1), Ordering::Relaxed);
        progress.set_status(format!("Exporting audio ({})", settings.format.label()));
    }
    a.measure(&s, sources, &cancelled)?;
    *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = a.loudness;
    let io = |e: std::io::Error| ExportError::Io(e.to_string());
    let mut out = Out::create(settings)?;
    while let Some(planar) = a.pull(a.out.saturating_add(chunk), sources) {
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        let refs: Vec<&[f32]> = planar.iter().map(Vec::as_slice).collect();
        out.write_all(&enc.encode(&refs)).map_err(io)?;
        if !settings.part_of_batch {
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    out.write_all(&enc.flush()).map_err(io)?;
    let n = out.finish(settings)?;
    if settings.part_of_batch {
        progress.done.store(1, Ordering::Relaxed);
    }
    Ok((n, frames))
}
