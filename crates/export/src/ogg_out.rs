//! Opus: the sequence mix alone, CELT-only Opus from our encoder (48 kHz, mono or stereo) in an
//! Ogg file (`.opus`, RFC 7845). The first page holds `OpusHead` (with the encoder's pre-skip),
//! the second `OpusTags` (the export metadata as Vorbis comments), then the audio pages, each
//! with the granule position of its last packet; the final page's granule position trims the
//! padding of the last packet (§4.4), so the file is exactly as long as the range.
//!
//! Ogg Vorbis (`.ogg`, Vorbis I §A): our encoder at the settings' quality and sample rate; the
//! identification, comment and setup headers, then the audio, the last granule position again
//! trimming the padding. 5.1 is written in Vorbis channel order (L, C, R, Ls, Rs, LFE).

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use filmcraft_oggmux::{PageWriter, opus_tags, vorbis_comment};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::TimeRange;

use crate::audio_out::{AudioOut, finite};
use crate::{ExportError, ExportMetadata, ExportSettings, Out, Progress, Result};

/// The logical stream's serial number. One stream per file, so a constant keeps exports
/// byte-for-byte reproducible.
const SERIAL: u32 = 0x4643_4F50;

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
    let mut a = AudioOut::new(project.clone(), seq, settings, range)?;
    let mut enc = filmcraft_opusenc::Encoder::new(filmcraft_opusenc::EncoderConfig::new(a.channels, settings.opus_bitrate()))
        .map_err(|e| ExportError::Unsupported(format!("Opus: {e}")))?;
    let frames = a.remaining();
    // one second per chunk: memory stays flat however long the range is
    let chunk = i64::from(a.sr.max(1));
    if !settings.part_of_batch {
        progress.total.store(frames.div_ceil(chunk as u64).max(1), Ordering::Relaxed);
        progress.set_status(format!("Exporting audio ({})", settings.format.label()));
    }
    a.measure(settings, sources, &cancelled)?;
    *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = a.loudness;

    let io = |e: std::io::Error| ExportError::Io(e.to_string());
    let mut out = Out::create(settings)?;
    let mut pages = PageWriter::new(SERIAL);
    out.write_all(&pages.header(&enc.head().to_bytes())).map_err(io)?;
    let tags = settings.metadata.vorbis_comments();
    let tags: Vec<(&str, &str)> = tags.iter().map(|(k, v)| (*k, v.as_str())).collect();
    out.write_all(&pages.header(&opus_tags(&ExportMetadata::vendor(), &tags))).map_err(io)?;

    let frame = filmcraft_opusenc::FRAME_SIZE as i64;
    let mut granule = 0i64;
    let mut write = |packets: Vec<Vec<u8>>, out: &mut Out| -> Result<()> {
        for p in packets {
            granule = granule.saturating_add(frame);
            out.write_all(&pages.packet(&p, granule)).map_err(io)?;
        }
        Ok(())
    };
    while let Some(mut planar) = a.pull(a.out.saturating_add(chunk), sources) {
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        finite(&mut planar);
        let refs: Vec<&[f32]> = planar.iter().map(Vec::as_slice).collect();
        write(enc.encode(&refs).map_err(|e| ExportError::Encode(format!("Opus: {e}")))?, &mut out)?;
        if !settings.part_of_batch {
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    write(enc.finish().map_err(|e| ExportError::Encode(format!("Opus: {e}")))?, &mut out)?;
    // RFC 7845 §4.4: the last page's granule position ends the stream at the last real sample
    let end = i64::from(filmcraft_opusenc::PRE_SKIP).saturating_add(i64::try_from(enc.samples_in()).unwrap_or(i64::MAX));
    out.write_all(&pages.finish(Some(end))).map_err(io)?;
    let n = out.finish(settings)?;
    if settings.part_of_batch {
        progress.done.store(1, Ordering::Relaxed);
    }
    Ok((n, frames))
}

/// Our channel order (L, R, C, LFE, Ls, Rs for 5.1) to Vorbis order (L, C, R, Ls, Rs, LFE).
const VORBIS_51: [usize; 6] = [0, 2, 1, 4, 5, 3];

/// The channels of `planar` in Vorbis order (§4.3.9): 5.1 is reordered, other layouts are the same.
pub(crate) fn vorbis_order(planar: &[Vec<f32>]) -> Vec<&[f32]> {
    if planar.len() == 6 { VORBIS_51.iter().filter_map(|&c| planar.get(c).map(Vec::as_slice)).collect() } else { planar.iter().map(Vec::as_slice).collect() }
}

/// Encode the mix of `range` to `settings.path` as Ogg Vorbis; returns (bytes, samples per channel).
pub(crate) fn export_vorbis(
    project: &Arc<Project>,
    seq: ItemId,
    settings: &ExportSettings,
    range: TimeRange,
    sources: &dyn SourceProvider,
    progress: &Progress,
) -> Result<(u64, u64)> {
    let cancelled = || progress.cancel.load(Ordering::Relaxed);
    let mut a = AudioOut::new(project.clone(), seq, settings, range)?;
    let mut enc = filmcraft_vorbis::Encoder::new(filmcraft_vorbis::EncoderConfig::new(a.sr, a.channels, settings.vorbis_quality()))
        .map_err(|e| ExportError::Unsupported(format!("Vorbis: {e}")))?;
    let frames = a.remaining();
    let chunk = i64::from(a.sr.max(1));
    if !settings.part_of_batch {
        progress.total.store(frames.div_ceil(chunk as u64).max(1), Ordering::Relaxed);
        progress.set_status(format!("Exporting audio ({})", settings.format.label()));
    }
    a.measure(settings, sources, &cancelled)?;
    *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = a.loudness;

    let io = |e: std::io::Error| ExportError::Io(e.to_string());
    let mut out = Out::create(settings)?;
    let mut pages = PageWriter::new(SERIAL);
    let tags = settings.metadata.vorbis_comments();
    let tags: Vec<(&str, &str)> = tags.iter().map(|(k, v)| (*k, v.as_str())).collect();
    out.write_all(&pages.header(&enc.identification())).map_err(io)?;
    // the comment and setup headers share a page that ends before the audio (§A.2)
    out.write_all(&pages.packet(&vorbis_comment(&ExportMetadata::vendor(), &tags), 0)).map_err(io)?;
    out.write_all(&pages.packet(&enc.setup(), 0)).map_err(io)?;
    out.write_all(&pages.flush()).map_err(io)?;

    let mut write = |packets: Vec<(Vec<u8>, i64)>, out: &mut Out| -> Result<()> {
        for (p, granule) in packets {
            out.write_all(&pages.packet(&p, granule)).map_err(io)?;
        }
        Ok(())
    };
    while let Some(mut planar) = a.pull(a.out.saturating_add(chunk), sources) {
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        finite(&mut planar);
        let refs = vorbis_order(&planar);
        write(enc.encode(&refs).map_err(|e| ExportError::Encode(format!("Vorbis: {e}")))?, &mut out)?;
        if !settings.part_of_batch {
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    write(enc.finish().map_err(|e| ExportError::Encode(format!("Vorbis: {e}")))?, &mut out)?;
    out.write_all(&pages.finish(None)).map_err(io)?;
    let n = out.finish(settings)?;
    if settings.part_of_batch {
        progress.done.store(1, Ordering::Relaxed);
    }
    Ok((n, frames))
}
