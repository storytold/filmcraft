//! Ogg audio source (`craft_media::ogg` demux): Ogg Opus with our Opus decoder and Ogg Vorbis with the
//! bootstrap Vorbis decoder.
//!
//! Opus (RFC 7845): packet positions come from granule positions and TOC durations
//! ([`craft_media::ogg::OpusTiming`]); the pre-skip samples land before 0 and are never returned, the
//! final page's end trimming is applied, and random access decodes [`crate::audio::OPUS_PRE_ROLL`]
//! of preceding packets first (so a seek gives the same samples as continuous playback).
//!
//! Vorbis: decoded once when opened (audio files are small next to video), aligned to the granule
//! positions (leading samples before the first page's granule and samples after the last page's
//! granule are dropped).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use craft_media::ogg::{Codec, OggFile, OpusTiming};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource};
use filmcraft_time::Tick;

use crate::CodecError;
use crate::audio::{OPUS_PRE_ROLL, OPUS_RATE, PacketDecoder, check_decoded_budget, read_resampled};

pub fn sniff(head: &[u8]) -> bool {
    craft_media::ogg::sniff(head)
}

struct OpusState {
    decoder: Option<PacketDecoder>,
    packets: HashMap<usize, Arc<Vec<Vec<f32>>>>,
    order: Vec<usize>,
    last_decoded: Option<usize>,
}

enum Kind {
    Opus {
        head: Box<craft_audio::opus::OpusHead>,
        timing: OpusTiming,
        state: Mutex<OpusState>,
    },
    /// Fully decoded planar samples.
    Pcm {
        rate: u32,
        samples: Vec<Vec<f32>>,
    },
}

pub struct OggSource {
    info: MediaInfo,
    bytes: crate::Src,
    file: OggFile,
    stream: usize,
    kind: Kind,
}

impl OggSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bytes = crate::Src(reader);
        let file = craft_media::ogg::open(&bytes).map_err(|e| CodecError::Container(e.to_string()))?;
        for w in &file.warnings {
            log::warn!("{name}: {w}");
        }
        let stream = file
            .streams
            .iter()
            .position(|s| matches!(s.codec, Codec::Opus | Codec::Vorbis) && !s.packets.is_empty())
            .ok_or_else(|| CodecError::Unsupported("Ogg: no Opus or Vorbis stream".into()))?;
        let st = &file.streams[stream];
        let (kind, rate, channels, codec) = match st.codec {
            Codec::Opus => {
                // an oversized first packet is filed as data, leaving an Opus stream without its header
                let first = st.headers.first().ok_or_else(|| CodecError::Decode("Opus: missing header packet".into()))?;
                let head = craft_audio::opus::OpusHead::parse(first).map_err(|e| CodecError::Unsupported(format!("Opus: {e}")))?;
                let timing = OpusTiming::of(st, head.pre_skip as u32);
                let channels = head.channels as u32;
                (
                    Kind::Opus {
                        head: Box::new(head),
                        timing,
                        state: Mutex::new(OpusState { decoder: None, packets: HashMap::new(), order: Vec::new(), last_decoded: None }),
                    },
                    OPUS_RATE,
                    channels,
                    "Opus",
                )
            }
            _ => {
                let (rate, samples) = decode_vorbis(&file, &bytes, stream)?;
                let ch = samples.len() as u32;
                (Kind::Pcm { rate, samples }, rate, ch, "Vorbis")
            }
        };
        let frames = match &kind {
            Kind::Opus { timing, .. } => timing.total,
            Kind::Pcm { samples, .. } => samples.first().map_or(0, Vec::len) as i64,
        };
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::AudioOnly,
            duration: Tick::from_units(frames, rate as i64),
            video: None,
            audio_streams: vec![AudioStreamInfo { sample_rate: rate, channels: channels.max(1), codec: codec.into(), bits_per_sample: None }],
            container: "Ogg".into(),
            start_timecode: None,
            file_size: Some(bytes.0.len()),
        };
        Ok(Self { info, bytes, file, stream, kind })
    }

    /// The demuxed file.
    pub fn file(&self) -> &OggFile {
        &self.file
    }

    fn opus_packet(&self, st: &mut OpusState, head: &craft_audio::opus::OpusHead, timing: &OpusTiming, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
        if let Some(p) = st.packets.get(&i) {
            return Ok(p.clone());
        }
        let read = |k: usize| self.file.read_packet(&self.bytes, self.stream, k).map_err(|e| CodecError::Container(e.to_string()));
        if st.decoder.is_none() {
            st.decoder = Some(PacketDecoder::opus(head.clone())?);
        }
        // Non-sequential access: reset and decode the pre-roll (RFC 7845 §4.6; see OPUS_PRE_ROLL).
        if st.last_decoded.is_none_or(|l| l + 1 != i) {
            let d = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?;
            d.reset();
            let from = timing.starts.partition_point(|&x| x <= timing.starts[i].saturating_sub(OPUS_PRE_ROLL as i64)).saturating_sub(1);
            for j in from..i {
                if let Ok(prev) = read(j) {
                    let _ = d.decode(&prev, 0);
                }
            }
        }
        let data = read(i)?;
        let r = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?.decode(&data, 0);
        st.last_decoded = Some(i);
        let p = Arc::new(r.unwrap_or_default());
        st.packets.insert(i, p.clone());
        st.order.push(i);
        if st.order.len() > 2048 {
            let old = st.order.remove(0);
            st.packets.remove(&old);
        }
        Ok(p)
    }
}

/// Decode a Vorbis stream fully, aligned to its granule positions.
fn decode_vorbis(file: &OggFile, bytes: &crate::Src, s: usize) -> crate::Result<(u32, Vec<Vec<f32>>)> {
    use symphonia::core::codecs::CODEC_TYPE_VORBIS;
    let st = &file.streams[s];
    if st.headers.len() < 3 {
        return Err(CodecError::Decode("Vorbis: missing header packets".into()));
    }
    let vi = craft_media::ogg::VorbisInfo::parse(&st.headers[0]).ok_or_else(|| CodecError::Decode("Vorbis: bad identification header".into()))?;
    let mut extra = st.headers[0].clone();
    extra.extend_from_slice(&st.headers[2]);
    let mut dec = PacketDecoder::new(CODEC_TYPE_VORBIS, vi.sample_rate, Some(extra))?;
    let ch = vi.channels.max(1) as usize;
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); ch];
    let mut lead: Option<i64> = None;
    for (i, p) in st.packets.iter().enumerate() {
        let data = file.read_packet(bytes, s, i).map_err(|e| CodecError::Container(e.to_string()))?;
        if let Ok(pcm) = dec.decode(&data, 0) {
            check_decoded_budget(out.iter().map(Vec::len).sum(), pcm.iter().take(ch).map(Vec::len).sum())?;
            for (c, o) in out.iter_mut().enumerate() {
                if let Some(src) = pcm.get(c) {
                    o.extend_from_slice(src);
                }
            }
        }
        if lead.is_none()
            && let Some(g) = p.granule
        {
            // samples decoded through the first granule page beyond its granule precede time 0
            lead = Some((out[0].len() as i64 - g).max(0));
        }
    }
    let lead = lead.unwrap_or(0) as usize;
    for o in &mut out {
        o.drain(..lead.min(o.len()));
    }
    if st.saw_eos
        && let Some(g) = st.last_granule
    {
        for o in &mut out {
            o.truncate(g.max(0) as usize);
        }
    }
    Ok((vi.sample_rate, out))
}

impl MediaSource for OggSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        Err(MediaError::NoStream("video"))
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let (head, timing, state) = match &self.kind {
            Kind::Pcm { rate, samples } => return read_resampled(samples, *rate, start, frames, sample_rate),
            Kind::Opus { head, timing, state } => (head, timing, state),
        };
        if sample_rate == 0 {
            return Err(MediaError::Decode("audio sample rate must be nonzero".into()));
        }
        let ch = head.channels.max(1);
        // Bound the request before the Opus frame count is derived from it (a rate of 1 Hz at most scales it by 48000).
        filmcraft_media::validate_audio_buffer_shape(ch, frames)?;
        let ratio = OPUS_RATE as f64 / sample_rate as f64;
        let exact = sample_rate == OPUS_RATE;
        let s0 = if exact { start } else { (start as f64 * ratio).floor() as i64 };
        let need = if exact { frames as i64 } else { (frames as f64 * ratio).ceil() as i64 + 2 };
        // OpusHead may declare up to 255 channels; the shared limit also bounds both buffers
        filmcraft_media::validate_audio_buffer_shape(ch, need.max(0) as usize)?;
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        let n = timing.starts.len();
        let mut i = timing.starts.partition_point(|&x| x <= s0.max(0)).saturating_sub(1);
        while i < n {
            let pk_start = timing.starts[i];
            if pk_start >= s0 + need || pk_start >= timing.total {
                break;
            }
            let pk = self.opus_packet(&mut st, head, timing, i)?;
            for (c, dst) in src.iter_mut().enumerate() {
                let Some(chan) = pk.get(c.min(pk.len().saturating_sub(1))) else { continue };
                for (k, v) in chan.iter().enumerate() {
                    let pos = pk_start + k as i64;
                    // before 0: pre-skip; at or after total: end trimming
                    if pos < 0 || pos >= timing.total {
                        continue;
                    }
                    let at = pos - s0;
                    if at >= 0 && (at as usize) < dst.len() {
                        dst[at as usize] = *v;
                    }
                }
            }
            i += 1;
        }
        drop(st);
        let mut out = AudioBuffer::silence(sample_rate, ch, frames);
        if exact {
            out.channels = src;
            return Ok(out);
        }
        filmcraft_media::resample_window_into(&src, s0, OPUS_RATE, start, sample_rate, &mut out.channels);
        Ok(out)
    }
}

/// Ogg Opus / Vorbis; other Ogg codecs (FLAC) fall through to the bootstrap audio opener.
fn handles(head: &[u8]) -> bool {
    sniff(head) && (head.windows(8).any(|w| w == b"OpusHead") || head.windows(7).any(|w| w == b"\x01vorbis"))
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !handles(&bytes[..bytes.len().min(4096)]) {
        return None;
    }
    Some(OggSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

/// [`filmcraft_media::ReaderOpener`] for Ogg Opus / Vorbis.
pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !handles(&head[..head.len().min(4096)]) {
        return None;
    }
    Some(OggSource::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

#[cfg(test)]
mod cut_tests {
    use super::*;

    /// Ogg CRC-32 (polynomial 0x04C11DB7, no reflection, no final XOR; RFC 3533).
    fn crc(data: &[u8]) -> u32 {
        let mut crc = 0u32;
        for &b in data {
            crc ^= u32::from(b) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
            }
        }
        crc
    }

    /// One page holding whole packets (each under 255 bytes).
    fn page(out: &mut Vec<u8>, flags: u8, granule: i64, seq: u32, packets: &[&[u8]]) {
        let start = out.len();
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(flags);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&9u32.to_le_bytes());
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.push(packets.len() as u8);
        out.extend(packets.iter().map(|p| p.len() as u8));
        for p in packets {
            out.extend_from_slice(p);
        }
        let sum = crc(&out[start..]);
        out[start + 22..start + 26].copy_from_slice(&sum.to_le_bytes());
    }

    /// An Ogg Opus file of CELT 20 ms packets with pseudo-random payloads, 3 packets a page.
    fn opus_file(packets: usize) -> Vec<u8> {
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1, 0, 0]);
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]);
        let mut f = Vec::new();
        page(&mut f, 0x02, 0, 0, &[&head]);
        page(&mut f, 0, 0, 1, &[b"OpusTags\0\0\0\0\0\0\0\0"]);
        let noise = filmcraft_testkit::cuts::noise(packets * 120, 6);
        let pkts: Vec<Vec<u8>> = noise
            .chunks(120)
            .map(|c| {
                let mut p = vec![0xF8];
                p.extend(c.iter().map(|v| (v * 127.0 + 128.0) as u8));
                p
            })
            .collect();
        let mut done = 0;
        for (seq, chunk) in (2u32..).zip(pkts.chunks(3)) {
            done += chunk.len();
            let refs: Vec<&[u8]> = chunk.iter().map(Vec::as_slice).collect();
            page(&mut f, if done == packets { 0x04 } else { 0 }, done as i64 * 960, seq, &refs);
        }
        f
    }

    /// Resampled audio must not depend on where a read starts (#4673): Opus (48 kHz) read at 47 kHz.
    #[test]
    fn resampled_opus_does_not_depend_on_the_read_cuts() {
        let src = OggSource::open("cuts.opus", opus_file(30).into()).unwrap();
        filmcraft_testkit::cuts::assert_cut_independent("Opus 48 kHz at 47 kHz", 1_000, 20_000, |s, n| src.audio(s, n, 47_000).unwrap().channels);
    }
}
