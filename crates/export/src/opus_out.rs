//! Ogg Opus export: headers, encoder pre-skip, and final granule trimming (RFC 7845).
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use filmcraft_isobmff::CodecConfig;
use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::TimeRange;

use crate::audio_out::AudioOut;
use crate::ogg::{PageWriter, opus_tags};
use crate::{ExportError, ExportSettings, Format, Out, Progress, Result, audio_factories};

pub(crate) fn export(
    project: &Arc<Project>,
    seq: ItemId,
    settings: &ExportSettings,
    range: TimeRange,
    sources: &dyn SourceProvider,
    progress: &Progress,
) -> Result<(u64, u64)> {
    let cancelled = || progress.cancel.load(Ordering::Relaxed);
    let mut audio = AudioOut::new(project.clone(), seq, settings, range)?;
    let mut enc = audio_factories()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find_map(|f| f(Format::Opus, audio.sr, audio.channels as u32, settings))
        .ok_or_else(|| ExportError::Unsupported("Opus encoder not available".into()))??;
    let entry = enc.sample_entry();
    let CodecConfig::Opus(cfg) = &entry.codec else {
        return Err(ExportError::Encode("Opus encoder returned another codec".into()));
    };
    let frames = audio.remaining();
    let chunk = i64::from(audio.sr);
    if !settings.part_of_batch {
        progress.total.store(frames.div_ceil(chunk as u64).max(1), Ordering::Relaxed);
    }
    audio.measure(settings, sources, &cancelled)?;
    *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = audio.loudness;
    let io = |e: std::io::Error| ExportError::Io(e.to_string());
    let mut out = Out::create(settings)?;
    let mut pages = PageWriter::new(0x46434f50);
    out.write_all(&pages.header(&cfg.opus_head())).map_err(io)?;
    out.write_all(&pages.header(&opus_tags("FilmCraft", &[]))).map_err(io)?;
    let mut granule = 0i64;
    let size = enc.frame_size();
    let mut write = |packets: Vec<Vec<u8>>, out: &mut Out| -> Result<()> {
        for packet in packets {
            granule = granule.saturating_add(i64::from(size));
            out.write_all(&pages.packet(&packet, granule)).map_err(io)?;
        }
        Ok(())
    };
    while let Some(planar) = audio.pull(audio.out.saturating_add(chunk), sources) {
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        write(enc.encode(&planar)?, &mut out)?;
        if !settings.part_of_batch {
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    write(enc.flush()?, &mut out)?;
    let end = i64::try_from(frames).unwrap_or(i64::MAX).saturating_add(i64::from(enc.priming()));
    out.write_all(&pages.finish(Some(end))).map_err(io)?;
    Ok((out.finish(settings)?, frames))
}
