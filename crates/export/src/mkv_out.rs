//! Matroska (`.mkv`) multiplexing for the stepped exporter: SimpleBlocks / BlockGroups through
//! craft-media's [`MkvWriter`], with the codec private data each track needs.
//!
//! Video: H.264 (`V_MPEG4/ISO/AVC` + `avcC`), H.265 (`V_MPEGH/ISO/HEVC` + `hvcC`), AV1
//! (`V_AV1` + `av1C`), ProRes (`V_PRORES` + the four-character code) and Motion-JPEG (`V_MJPEG`).
//! Audio: AAC (`A_AAC` + the AudioSpecificConfig), FLAC (`A_FLAC` + `fLaC` and the STREAMINFO
//! metadata block) and uncompressed PCM (`A_PCM/INT/LIT` or `A_PCM/FLOAT/IEEE`, written in blocks of
//! [`PCM_BLOCK`] samples with exact timestamps). Encoded access units come from the export's audio
//! encoder; each block's timestamp is its first sample.

use std::io::{Seek, Write};

use craft_media::matroska::{LacingMode, MkvWriter, MuxOptions, TrackKind, TrackSpec};
use filmcraft_isobmff::{CodecConfig, SampleEntry};
use filmcraft_time::FrameRate;

use crate::output::Out;
use crate::pcm::{self, PcmContainer};
use crate::{AudioCodec, EncodedPacket, ExportError, ExportSettings, Format, Result, SampleFormat};

/// Samples per PCM block: small enough for seekable, cheap-to-skip audio, large enough that the
/// per-block overhead stays far below the sample data.
pub const PCM_BLOCK: usize = 1024;

/// The Matroska track writer of one export.
pub struct MkvMux {
    w: MkvWriter<Out>,
    vt: usize,
    at: Option<usize>,
    rate: FrameRate,
    frame_duration_ns: u64,
    packets: i64,
    audio: Option<AudioTrack>,
}

/// An encoded audio track's initial state, read from its encoder.
pub struct EncodedAudio<'a> {
    /// The encoder's sample entry (AAC AudioSpecificConfig, FLAC STREAMINFO).
    pub entry: &'a SampleEntry,
    /// Samples per access unit.
    pub frame_size: u32,
    /// Encoder delay in samples (AAC priming).
    pub priming: u32,
}

enum AudioTrack {
    /// Uncompressed samples written by [`MkvMux::write_audio`].
    Pcm {
        sr: u32,
        channels: u32,
        format: SampleFormat,
        /// Samples written so far across the whole track.
        written: u64,
    },
    /// Access units written by [`MkvMux::write_encoded_audio`].
    Encoded { sr: u32, written: u64 },
}

fn io(e: impl std::fmt::Display) -> ExportError {
    ExportError::Io(format!("Matroska: {e}"))
}

/// `count` / `per_second` in nanoseconds, rounded to the nearest nanosecond.
fn ns(count: i128, per_second: i128) -> Result<i64> {
    if per_second <= 0 {
        return Err(ExportError::Unsupported("Matroska: the frame rate or sample rate is zero".into()));
    }
    let v = (count.saturating_mul(1_000_000_000) + per_second / 2) / per_second;
    i64::try_from(v).map_err(|_| ExportError::Unsupported("Matroska: a timestamp does not fit in 64 bits".into()))
}

/// The codec id and CodecPrivate bytes of the video track.
fn video_codec(settings: &ExportSettings, entry: &SampleEntry) -> Result<(&'static str, Vec<u8>)> {
    match (settings.video_format(), &entry.codec) {
        (Format::H264, CodecConfig::Avc(c)) => Ok(("V_MPEG4/ISO/AVC", c.to_bytes())),
        (Format::Hevc, CodecConfig::Hevc(c)) => Ok(("V_MPEGH/ISO/HEVC", c.to_bytes())),
        (Format::Av1, CodecConfig::Av1(c)) => Ok(("V_AV1", c.to_bytes())),
        (Format::ProRes, CodecConfig::ProRes { fourcc }) => Ok(("V_PRORES", fourcc.0.to_vec())),
        (Format::Mjpeg, _) => Ok(("V_MJPEG", Vec::new())),
        (format, _) => Err(ExportError::Unsupported(format!("Matroska cannot carry {}", format.label()))),
    }
}

/// The Matroska track spec and writer state for the mixed audio track.
fn audio_track_spec(settings: &ExportSettings, sr: u32, channels: u32, encoded: Option<EncodedAudio<'_>>) -> Result<(TrackSpec, AudioTrack)> {
    let sr = sr.max(1);
    let channels = channels.max(1);
    let mut a = TrackSpec::new(TrackKind::Audio, "");
    match settings.audio_codec() {
        AudioCodec::Pcm => {
            let format = settings.audio.sample_format();
            a.codec_id = if format.is_float() { "A_PCM/FLOAT/IEEE" } else { "A_PCM/INT/LIT" }.into();
            a.audio = Some((f64::from(sr), u64::from(channels), Some(u64::from(format.bits()))));
            a.default_duration_ns = Some(block_duration_ns(
                u32::try_from(PCM_BLOCK).map_err(|_| ExportError::Unsupported("Matroska: the PCM block size does not fit".into()))?,
                sr,
            )?);
            Ok((a, AudioTrack::Pcm { sr, channels, format, written: 0 }))
        }
        codec @ (AudioCodec::Aac | AudioCodec::Flac) => {
            let enc = encoded.ok_or_else(|| ExportError::Unsupported(format!("Matroska: {codec:?} audio needs an encoder")))?;
            let (id, private, bits) = match (codec, &enc.entry.codec) {
                (AudioCodec::Aac, CodecConfig::Aac(cfg)) => ("A_AAC", cfg.asc.clone(), None),
                (AudioCodec::Flac, CodecConfig::Flac(cfg)) => {
                    // Matroska stores the `fLaC` stream marker and the STREAMINFO metadata block.
                    let mut p = b"fLaC".to_vec();
                    p.extend_from_slice(&cfg.metadata_blocks);
                    ("A_FLAC", p, Some(u64::from(cfg.bits_per_sample.max(1))))
                }
                (_, _) => {
                    return Err(ExportError::Unsupported("Matroska: the audio encoder's sample entry does not match the chosen codec".into()));
                }
            };
            a.codec_id = id.into();
            a.codec_private = private;
            a.audio = Some((f64::from(sr), u64::from(channels), bits));
            if enc.frame_size > 0 {
                a.default_duration_ns = Some(block_duration_ns(enc.frame_size, sr)?);
            }
            a.codec_delay_ns = u64::try_from(ns(i128::from(enc.priming), i128::from(sr))?).unwrap_or(0);
            Ok((a, AudioTrack::Encoded { sr, written: 0 }))
        }
        AudioCodec::Auto => Err(ExportError::Unsupported("Matroska: the audio codec must be resolved before writing".into())),
    }
}

/// The duration of `samples` at `sr` in nanoseconds.
fn block_duration_ns(samples: u32, sr: u32) -> Result<u64> {
    u64::try_from(ns(i128::from(samples), i128::from(sr.max(1)))?).map_err(|_| ExportError::Unsupported("Matroska: an audio block duration is negative".into()))
}

impl MkvMux {
    /// Create the file and its tracks. `entry` is the video encoder's sample entry (the source of
    /// the codec configuration records); `audio` is the mixed track's `(sample rate, channels)` and
    /// `encoded` its encoder's configuration when the audio is AAC or FLAC (PCM otherwise).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        settings: &ExportSettings,
        name: &str,
        width: u32,
        height: u32,
        rate: FrameRate,
        entry: &SampleEntry,
        audio: Option<(u32, u32)>,
        encoded: Option<EncodedAudio<'_>>,
    ) -> Result<Self> {
        if rate.num <= 0 || rate.den <= 0 {
            return Err(ExportError::Unsupported("Matroska: the output frame rate must be positive".into()));
        }
        let out = Out::create(settings)?;
        let frame_duration_ns = ns(i128::from(rate.den), i128::from(rate.num))?;
        let frame_duration_ns = u64::try_from(frame_duration_ns).map_err(|_| ExportError::Unsupported("Matroska: the frame duration is negative".into()))?;
        let (codec_id, private) = video_codec(settings, entry)?;
        let mut video = TrackSpec::new(TrackKind::Video, codec_id);
        video.codec_private = private;
        video.default_duration_ns = Some(frame_duration_ns);
        video.video_size = Some((width, height));
        if !name.is_empty() {
            video.name = Some(name.to_string());
        }
        let mut tracks = vec![video];
        let mut audio_track = None;
        if let Some((sr, channels)) = audio {
            let (spec, track) = audio_track_spec(settings, sr, channels, encoded)?;
            tracks.push(spec);
            audio_track = Some(track);
        }
        let at = audio_track.as_ref().map(|_| 1);
        let mut opts = MuxOptions {
            doc_type: "matroska".into(),
            writing_app: "FilmCraft".into(),
            title: (!name.is_empty()).then(|| name.to_string()),
            ..MuxOptions::default()
        };
        opts.lacing = LacingMode::None;
        let w = MkvWriter::new(out, tracks, opts).map_err(io)?;
        Ok(MkvMux { w, vt: 0, at, rate, frame_duration_ns, packets: 0, audio: audio_track })
    }

    pub fn write_video(&mut self, packets: Vec<EncodedPacket>) -> Result<()> {
        for p in packets {
            let ticks = i128::from(self.packets).saturating_mul(i128::from(self.rate.den)).saturating_add(i128::from(p.composition_offset));
            let pts = ns(ticks, i128::from(self.rate.num))?;
            let duration = if p.duration == 0 {
                Some(self.frame_duration_ns)
            } else {
                Some(u64::try_from(ns(i128::from(p.duration), i128::from(self.rate.num))?).unwrap_or(self.frame_duration_ns))
            };
            self.w.write_frame(self.vt, pts, p.key, &p.data, duration).map_err(io)?;
            self.packets = self.packets.saturating_add(1);
        }
        Ok(())
    }

    /// Write one call's worth of uncompressed samples (PCM tracks only).
    pub fn write_audio(&mut self, planar: &[Vec<f32>]) -> Result<()> {
        let Some(at) = self.at else { return Ok(()) };
        let Some(AudioTrack::Pcm { sr, channels, format, .. }) = self.audio.as_ref() else {
            if self.audio.is_some() {
                return Err(ExportError::Unsupported("Matroska: encoded audio is written as access units".into()));
            }
            return Ok(());
        };
        let (sr, channels, format) = (*sr, *channels, *format);
        let frames = planar.first().map_or(0, Vec::len);
        if frames == 0 {
            return Ok(());
        }
        let (sr, channels) = (sr.max(1), channels.max(1));
        let interleaved = crate::audio_out::interleave(planar);
        let mut bytes = Vec::new();
        pcm::encode(PcmContainer::Wav, &interleaved, format, &mut bytes);
        let block_bytes = (usize::try_from(channels).unwrap_or(0)).saturating_mul(format.bytes()).max(1);
        let mut offset = 0usize;
        while offset < frames {
            let n = PCM_BLOCK.min(frames - offset);
            let start = offset.saturating_mul(block_bytes);
            let end = offset.saturating_add(n).saturating_mul(block_bytes).min(bytes.len());
            let Some(data) = bytes.get(start..end) else { break };
            let written = self
                .audio
                .as_ref()
                .map_or(0, |a| match a {
                    AudioTrack::Pcm { written, .. } => *written,
                    AudioTrack::Encoded { .. } => 0,
                })
                .saturating_add(offset as u64);
            let pts = ns(i128::from(written), i128::from(sr))?;
            let duration = u64::try_from(ns(i128::from(n as u64), i128::from(sr))?).unwrap_or(0);
            self.w.write_frame(at, pts, true, data, Some(duration)).map_err(io)?;
            offset = offset.saturating_add(n);
        }
        if let Some(AudioTrack::Pcm { written, .. }) = self.audio.as_mut() {
            *written = written.saturating_add(frames as u64);
        }
        Ok(())
    }

    /// Write encoded access units with the number of samples each decodes to (AAC 1024, FLAC 4096,
    /// the last FLAC frame shorter). Each block's timestamp is its first sample.
    pub fn write_encoded_audio(&mut self, aus: &[(Vec<u8>, u32)]) -> Result<()> {
        let Some(at) = self.at else { return Ok(()) };
        let Some(AudioTrack::Encoded { sr, .. }) = self.audio.as_ref() else {
            if self.audio.is_some() {
                return Err(ExportError::Unsupported("Matroska: uncompressed audio is written as PCM blocks".into()));
            }
            return Ok(());
        };
        let sr = (*sr).max(1);
        let mut written = match self.audio.as_ref() {
            Some(AudioTrack::Encoded { written, .. }) => *written,
            _ => 0,
        };
        for (au, samples) in aus {
            if au.is_empty() {
                continue;
            }
            let pts = ns(i128::from(written), i128::from(sr))?;
            let duration = u64::try_from(ns(i128::from(*samples), i128::from(sr))?).unwrap_or(0);
            self.w.write_frame(at, pts, true, au, Some(duration)).map_err(io)?;
            written = written.saturating_add(u64::from(*samples));
        }
        if let Some(AudioTrack::Encoded { written: w, .. }) = self.audio.as_mut() {
            *w = written;
        }
        Ok(())
    }

    pub fn finish(self, settings: &ExportSettings) -> Result<(u64, Vec<String>)> {
        let out = self.w.finish().map_err(io)?;
        Ok((out.finish(settings)?, Vec::new()))
    }
}

// Keep the `Seek` import exercised: `MkvWriter<Out>` needs `Out: Write + Seek`.
const _: fn() = || {
    fn assert_seek<T: Write + Seek>() {}
    assert_seek::<Out>();
};
