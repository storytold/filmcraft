//! AAC Audio: the sequence mix alone, AAC-LC from the same encoder as the MPEG-4 movies, in an
//! MPEG-4 audio file (`.m4a`, the encoder delay signalled by an edit list) or, when the path ends
//! in `.aac`, a raw ADTS stream (ISO/IEC 14496-3 §1.A.2; one header per access unit, no CRC). ADTS
//! cannot signal the encoder delay, so a player starts it with the priming samples.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use filmcraft_isobmff::{Brand, CodecConfig, Mp4Writer, TrackConfig, WriteSample, WriterOptions};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::TimeRange;

use crate::audio_out::AudioOut;
use crate::{AudioEncoder, ExportError, ExportSettings, Format, Out, Progress, Result, audio_factories};

/// Whether `path` asks for a raw ADTS stream rather than an MPEG-4 file.
pub(crate) fn is_adts_path(path: &str) -> bool {
    std::path::Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("aac"))
}

/// Where the encoded access units go.
enum Sink {
    Mp4 { mux: Mp4Writer<Out>, track: usize, frame: u32 },
    Adts { out: Out, header: AdtsHeader },
}

impl Sink {
    fn write(&mut self, units: Vec<Vec<u8>>) -> Result<()> {
        let io = |e: std::io::Error| ExportError::Io(e.to_string());
        match self {
            Sink::Mp4 { mux, track, frame } => {
                for au in units {
                    mux.write_sample(*track, WriteSample { data: &au, duration: *frame, composition_offset: 0, is_sync: true })
                        .map_err(|e| ExportError::Io(e.to_string()))?;
                }
            }
            Sink::Adts { out, header } => {
                for au in units {
                    out.write_all(&header.bytes(au.len())?).map_err(io)?;
                    out.write_all(&au).map_err(io)?;
                }
            }
        }
        Ok(())
    }

    fn finish(self, settings: &ExportSettings) -> Result<u64> {
        match self {
            Sink::Mp4 { mux, .. } => mux.finish().map_err(|e| ExportError::Io(e.to_string()))?.finish(settings),
            Sink::Adts { out, .. } => out.finish(settings),
        }
    }
}

/// Validated ADTS configuration for the encoder's AudioSpecificConfig.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdtsHeader(craft_audio::aac::AdtsConfig);

impl AdtsHeader {
    pub(crate) fn from_asc(asc: &[u8]) -> Result<Self> {
        craft_audio::aac::AdtsConfig::from_asc_prefix(asc).map(Self).map_err(|e| match e {
            craft_audio::aac::AdtsConfigError::Short => ExportError::Encode("AAC: the encoder gave no AudioSpecificConfig".into()),
            craft_audio::aac::AdtsConfigError::Unsupported { object_type, sf_index, channel_config } => ExportError::Unsupported(format!(
                "an ADTS .aac stream cannot carry this AAC configuration (object type {object_type}, rate index {sf_index}, channels {channel_config}): export .m4a instead"
            )),
        })
    }

    pub(crate) fn bytes(&self, payload: usize) -> Result<[u8; 7]> {
        self.0.header(payload).map_err(|_| ExportError::Encode(format!("AAC: a {payload}-byte frame does not fit ADTS")))
    }
}

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
    let mut enc: Box<dyn AudioEncoder> = audio_factories()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find_map(|f| f(Format::Aac, a.sr, a.channels as u32, settings))
        .ok_or_else(|| ExportError::Unsupported("AAC encoder not available".into()))??;
    let entry = enc.sample_entry();
    let frames = a.remaining();
    // one second per chunk: memory stays flat however long the range is
    let chunk = i64::from(a.sr.max(1));
    if !settings.part_of_batch {
        progress.total.store(frames.div_ceil(chunk as u64).max(1), Ordering::Relaxed);
        progress.set_status(format!("Exporting audio ({})", settings.format.label()));
    }
    a.measure(settings, sources, &cancelled)?;
    *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = a.loudness;
    let mut sink = if is_adts_path(&settings.path) {
        let CodecConfig::Aac(cfg) = &entry.codec else {
            return Err(ExportError::Encode("AAC: the encoder's sample entry is not AAC".into()));
        };
        Sink::Adts { header: AdtsHeader::from_asc(&cfg.asc)?, out: Out::create(settings)? }
    } else {
        let mut opts = WriterOptions::new(Brand::Mp4);
        opts.metadata = settings.metadata.udta();
        let mut mux = Mp4Writer::new(Out::create(settings)?, opts).map_err(|e| ExportError::Io(e.to_string()))?;
        let mut track = TrackConfig::new(entry, a.sr);
        track.media_start = Some(i64::from(enc.priming()));
        let track = mux.add_track(track).map_err(|e| ExportError::Io(e.to_string()))?;
        Sink::Mp4 { mux, track, frame: enc.frame_size() }
    };
    while let Some(planar) = a.pull(a.out.saturating_add(chunk), sources) {
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        sink.write(enc.encode(&planar)?)?;
        if !settings.part_of_batch {
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    sink.write(enc.flush()?)?;
    let n = sink.finish(settings)?;
    if settings.part_of_batch {
        progress.done.store(1, Ordering::Relaxed);
    }
    Ok((n, frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adts_header_fields_follow_the_asc() {
        // AAC-LC (2), 48 kHz (index 3), stereo: 00010 0011 0010 000
        let h = AdtsHeader::from_asc(&[0x11, 0x90]).unwrap();
        assert_eq!((h.0.object_type - 1, h.0.sf_index, h.0.channel_config), (1, 3, 2));
        let b = h.bytes(371).unwrap();
        assert_eq!(&b[..2], &[0xFF, 0xF1]);
        assert_eq!(b[2] >> 6, 1);
        assert_eq!((b[2] >> 2) & 0x0F, 3);
        assert_eq!(((b[2] & 1) << 2) | (b[3] >> 6), 2);
        let len = (u32::from(b[3] & 0x03) << 11) | (u32::from(b[4]) << 3) | u32::from(b[5] >> 5);
        assert_eq!(len, 378);
        assert_eq!(((u32::from(b[5] & 0x1F)) << 6) | u32::from(b[6] >> 2), 0x7FF);
        assert_eq!(b[6] & 0x03, 0);
    }

    #[test]
    fn adts_refuses_what_it_cannot_signal() {
        assert!(AdtsHeader::from_asc(&[]).is_err());
        assert!(AdtsHeader::from_asc(&[0x11]).is_err());
        // explicit sampling frequency (index 15)
        assert!(AdtsHeader::from_asc(&[0x17, 0x90, 0, 0, 0]).is_err());
        // object type 5 (SBR) has no ADTS profile
        assert!(AdtsHeader::from_asc(&[0x29, 0x90]).is_err());
        let h = AdtsHeader::from_asc(&[0x11, 0x90]).unwrap();
        assert!(h.bytes(8184).is_ok());
        assert!(h.bytes(8185).is_err());
        assert!(h.bytes(usize::MAX).is_err());
    }

    #[test]
    fn only_a_dot_aac_path_is_adts() {
        assert!(is_adts_path("/x/mix.aac"));
        assert!(is_adts_path("mix.AAC"));
        assert!(!is_adts_path("mix.m4a"));
        assert!(!is_adts_path("aac"));
    }
}
