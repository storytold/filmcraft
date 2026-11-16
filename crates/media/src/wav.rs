//! Range-backed WAV source. RIFF parsing and PCM conversion use `craft_audio::wav_range`;
//! FilmCraft maps its metadata and resamples the requested audio into the project timeline.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::Tick;

use crate::reader::{MemReader, SharedReader};
use crate::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result};

pub use craft_audio::wav_range::{sniff, write_wav16};

const BLOCK_FRAMES: usize = 4096;
const MAX_SPAN_BYTES: u64 = craft_audio::wav_range::MAX_RANGE_BYTES;

struct Reader(SharedReader);
impl craft_media::ByteSource for Reader {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

/// A WAV file read in place, with sample ranges decoded on demand.
pub struct WavSource {
    info: MediaInfo,
    wav: craft_audio::wav_range::WavRange<Reader>,
}

impl WavSource {
    /// Parse a WAV file held in memory.
    pub fn parse(name: &str, bytes: Arc<[u8]>) -> Result<Self> {
        Self::open(name, Arc::new(MemReader(bytes)))
    }

    /// Open a WAV file through a random-access reader.
    pub fn open(name: &str, reader: SharedReader) -> Result<Self> {
        let len = reader.len();
        let wav =
            craft_audio::wav_range::WavRange::open(Reader(reader), crate::MAX_AUDIO_CHANNELS, crate::MAX_AUDIO_SAMPLE_RATE).map_err(|e| wav_error(name, e))?;
        let mut info = MediaInfo {
            name: name.into(),
            kind: MediaKind::AudioOnly,
            duration: Tick::from_units(i64::try_from(wav.frames).unwrap_or(i64::MAX), i64::from(wav.rate)),
            video: None,
            audio_streams: vec![AudioStreamInfo {
                sample_rate: wav.rate,
                channels: u32::from(wav.channels),
                codec: if wav.float { "PCM float".into() } else { "PCM".into() },
                bits_per_sample: Some(u32::from(wav.bits)),
            }],
            container: if wav.time_reference.is_some() { "Broadcast WAV".into() } else { "WAV".into() },
            start_timecode: None,
            file_size: Some(len),
        };
        if let Some(tr) = wav.time_reference {
            let r = info.frame_rate();
            let n = tr as i128 * r.num as i128;
            let d = wav.rate as i128 * r.den as i128;
            info.start_timecode = (n + d / 2).checked_div(d).and_then(|t| i64::try_from(t).ok());
        }
        Ok(Self { info, wav })
    }

    /// Broadcast Wave start time: samples since midnight.
    pub fn time_reference(&self) -> Option<u64> {
        self.wav.time_reference
    }
}

fn wav_error(name: &str, error: craft_audio::AudioError) -> MediaError {
    match error {
        craft_audio::AudioError::Io(e) => MediaError::Io(format!("{name}: {e}")),
        craft_audio::AudioError::Unsupported(e) => MediaError::Unsupported(format!("{name}: {e}")),
        e => MediaError::Decode(format!("{name}: {e}")),
    }
}

impl MediaSource for WavSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>> {
        Err(MediaError::NoStream("video"))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        let total = self.wav.frames;
        crate::validate_audio_buffer_shape(usize::from(self.wav.channels), frames)?;
        let mut out = AudioBuffer::silence(sample_rate, usize::from(self.wav.channels), frames);
        if total == 0 || sample_rate == 0 {
            return Ok(out);
        }
        // Linear-interpolating resample when rates differ (the audio crate provides sinc resampling).
        let ratio = self.wav.rate as f64 / sample_rate as f64;
        let src_of = |i: usize| start.saturating_add(i as i64) as f64 * ratio;
        let mut i = 0;
        while i < frames {
            let end = frames.min(i + BLOCK_FRAMES);
            let (lo, hi) = (src_of(i), src_of(end - 1));
            if hi < 0.0 {
                i = end;
                continue;
            }
            let first = lo.max(0.0).floor() as u64;
            if first >= total {
                break;
            }
            let last = (hi.floor() as u64).saturating_add(1).min(total - 1);
            let span = last.saturating_sub(first).saturating_add(1).saturating_mul(self.wav.frame_bytes() as u64);
            let block = if span <= MAX_SPAN_BYTES { Some(self.wav.read_frames(first, last).map_err(|e| wav_error(&self.info.name, e))?) } else { None };
            for slot in i..end {
                let src = src_of(slot);
                if src < 0.0 {
                    continue;
                }
                let i0 = src.floor() as u64;
                if i0 >= total {
                    break;
                }
                let i1 = (i0 + 1).min(total - 1);
                let fr = (src - i0 as f64) as f32;
                let (buf, base) = match &block {
                    Some(b) => (std::borrow::Cow::Borrowed(b.as_slice()), first),
                    None => (std::borrow::Cow::Owned(self.wav.read_frames(i0, i1).map_err(|e| wav_error(&self.info.name, e))?), i0),
                };
                let (k0, k1) = ((i0 - base) as usize, (i1 - base) as usize);
                for c in 0..usize::from(self.wav.channels) {
                    let a = self.wav.sample(&buf, k0, c);
                    let b = self.wav.sample(&buf, k1, c);
                    if let Some(s) = out.channels.get_mut(c).and_then(|ch| ch.get_mut(slot)) {
                        *s = a + (b - a) * fr;
                    }
                }
            }
            i = end;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_preserves_pcm_bytes_and_canonical_header() {
        let wav = write_wav16(&[-1.0, -0.5, 0.0, 0.5, 1.0], 1, 48_000);
        let expected: &[u8] = &[
            b'R', b'I', b'F', b'F', 46, 0, 0, 0, b'W', b'A', b'V', b'E', b'f', b'm', b't', b' ', 16, 0, 0, 0, 1, 0, 1, 0, 0x80, 0xbb, 0, 0, 0x00, 0x77, 0x01,
            0, 2, 0, 16, 0, b'd', b'a', b't', b'a', 10, 0, 0, 0, 1, 0x80, 0, 0xc0, 0, 0, 0, 0x40, 0xff, 0x7f,
        ];
        assert_eq!(wav, expected);
    }

    #[test]
    fn roundtrip() {
        let s: Vec<f32> = (0..200).map(|i| ((i as f32) * 0.1).sin() * 0.5).collect();
        let bytes = write_wav16(&s, 2, 48000);
        let src = WavSource::parse("t.wav", bytes.into()).unwrap();
        assert_eq!(src.info().audio().unwrap().channels, 2);
        let a = src.audio(0, 100, 48000).unwrap();
        assert!((a.channels[0][3] - s[6]).abs() < 1e-4);
        assert!((a.channels[1][3] - s[7]).abs() < 1e-4);
        assert_eq!(src.info().duration, Tick::from_units(100, 48000));
        assert_eq!(src.time_reference(), None);
        assert_eq!(src.info().start_timecode, None);
    }

    #[test]
    fn shared_reader_seeks_and_resamples_with_silence_outside_file() {
        let bytes = write_wav16(&[0.0, 0.5, 1.0], 1, 48_000);
        let src = WavSource::parse("seek.wav", bytes.into()).unwrap();
        let audio = src.audio(-1, 8, 96_000).unwrap();
        let samples = &audio.channels[0];
        assert_eq!(samples[0], 0.0);
        assert_eq!(samples[1], 0.0);
        assert!((samples[2] - 0.25).abs() < 0.0001);
        assert!((samples[3] - 0.5).abs() < 0.0001);
        assert!((samples[4] - 0.75).abs() < 0.0001);
        assert!((samples[5] - 1.0).abs() < 0.0001);
        assert_eq!(samples[7], 0.0);
    }

    /// Insert a chunk before `data`.
    fn with_chunk(wav: &[u8], id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = wav[..36].to_vec();
        v.extend_from_slice(id);
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v.extend_from_slice(&wav[36..]);
        let riff = (v.len() - 8) as u32;
        v[4..8].copy_from_slice(&riff.to_le_bytes());
        v
    }

    #[test]
    fn bwf_time_reference_from_bext() {
        let wav = write_wav16(&[0.0; 96], 2, 48_000);
        // 01:00:00:00 at 23.976 fps: 86 400 frames of 2002 samples
        let tr: u64 = 86_400 * 2002;
        let mut bext = vec![0u8; 602];
        bext[338..342].copy_from_slice(&(tr as u32).to_le_bytes());
        bext[342..346].copy_from_slice(&((tr >> 32) as u32).to_le_bytes());
        let src = WavSource::parse("bwf.wav", with_chunk(&wav, b"bext", &bext).into()).unwrap();
        assert_eq!(src.time_reference(), Some(tr));
        assert_eq!(src.info().frame_rate(), filmcraft_time::FrameRate::FPS_23_976);
        assert_eq!(src.info().start_timecode, Some(86_400));
        assert_eq!(src.info().container, "Broadcast WAV");
        // the audio is unaffected
        assert_eq!(src.info().duration, Tick::from_units(48, 48_000));
    }

    #[test]
    fn bwf_time_reference_from_ixml_and_large_values() {
        let wav = write_wav16(&[0.0; 4], 1, 48_000);
        let tr: u64 = (1 << 32) + 5;
        let xml = format!(
            "<?xml version=\"1.0\"?><BWFXML><BEXT><BWF_TIME_REFERENCE_LOW>{}</BWF_TIME_REFERENCE_LOW><BWF_TIME_REFERENCE_HIGH>{}</BWF_TIME_REFERENCE_HIGH></BEXT></BWFXML>",
            tr & 0xFFFF_FFFF,
            tr >> 32
        );
        let src = WavSource::parse("ixml.wav", with_chunk(&wav, b"iXML", xml.as_bytes()).into()).unwrap();
        assert_eq!(src.time_reference(), Some(tr));
        // a short bext (no TimeReference) is ignored
        let src = WavSource::parse("short.wav", with_chunk(&wav, b"bext", &[0u8; 100]).into()).unwrap();
        assert_eq!(src.time_reference(), None);
    }

    #[test]
    fn hostile_header_dimensions_are_rejected_before_publishing_or_reading() {
        let mut wav = write_wav16(&[0.0; 2], 1, 48_000);
        // `fmt ` starts at byte 12: channels at +22, rate at +24.
        wav[22..24].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(WavSource::parse("wide.wav", wav.into()).is_err());
        let mut wav = write_wav16(&[0.0; 2], 1, 48_000);
        wav[24..28].copy_from_slice(&(crate::MAX_AUDIO_SAMPLE_RATE + 1).to_le_bytes());
        assert!(WavSource::parse("fast.wav", wav.into()).is_err());
    }
}
