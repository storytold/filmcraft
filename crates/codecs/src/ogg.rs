//! Ogg audio source (`filmcraft-ogg` demux): Ogg Opus with our Opus decoder and Ogg Vorbis with the
//! bootstrap Vorbis decoder.
//!
//! Opus (RFC 7845): packet positions come from granule positions and TOC durations
//! ([`filmcraft_ogg::OpusTiming`]); the pre-skip samples land before 0 and are never returned, the
//! final page's end trimming is applied, and random access decodes [`crate::audio::OPUS_PRE_ROLL`]
//! of preceding packets first (so a seek gives the same samples as continuous playback).
//!
//! Vorbis: decoded once when opened (audio files are small next to video), aligned to the granule
//! positions (leading samples before the first page's granule and samples after the last page's
//! granule are dropped).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource};
use filmcraft_ogg::{Codec, OggFile, OpusTiming};
use filmcraft_time::Tick;

use crate::CodecError;
use crate::audio::{OPUS_PRE_ROLL, OPUS_RATE, PacketDecoder, read_resampled};

impl filmcraft_ogg::ByteSource for crate::Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

pub fn sniff(head: &[u8]) -> bool {
    filmcraft_ogg::sniff(head)
}

struct OpusState {
    decoder: Option<PacketDecoder>,
    packets: HashMap<usize, Arc<Vec<Vec<f32>>>>,
    order: Vec<usize>,
    last_decoded: Option<usize>,
}

enum Kind {
    Opus {
        head: Box<filmcraft_opus::OpusHead>,
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
        let file = filmcraft_ogg::open(&bytes).map_err(|e| CodecError::Container(e.to_string()))?;
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
                let head = filmcraft_opus::OpusHead::parse(&st.headers[0]).map_err(|e| CodecError::Unsupported(format!("Opus: {e}")))?;
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

    fn opus_packet(&self, st: &mut OpusState, head: &filmcraft_opus::OpusHead, timing: &OpusTiming, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
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
            let from = timing.starts.partition_point(|&x| x <= timing.starts[i] - OPUS_PRE_ROLL as i64).saturating_sub(1);
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
    let vi = filmcraft_ogg::VorbisInfo::parse(&st.headers[0]).ok_or_else(|| CodecError::Decode("Vorbis: bad identification header".into()))?;
    let mut extra = st.headers[0].clone();
    extra.extend_from_slice(&st.headers[2]);
    let mut dec = PacketDecoder::new(CODEC_TYPE_VORBIS, vi.sample_rate, Some(extra))?;
    let ch = vi.channels.max(1) as usize;
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); ch];
    let mut lead: Option<i64> = None;
    for (i, p) in st.packets.iter().enumerate() {
        let data = file.read_packet(bytes, s, i).map_err(|e| CodecError::Container(e.to_string()))?;
        if let Ok(pcm) = dec.decode(&data, 0) {
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
            Kind::Pcm { rate, samples } => return Ok(read_resampled(samples, *rate, start, frames, sample_rate)),
            Kind::Opus { head, timing, state } => (head, timing, state),
        };
        let ch = head.channels.max(1);
        let ratio = OPUS_RATE as f64 / sample_rate as f64;
        let exact = sample_rate == OPUS_RATE;
        let s0 = if exact { start } else { (start as f64 * ratio).floor() as i64 };
        let need = if exact { frames as i64 } else { (frames as f64 * ratio).ceil() as i64 + 2 };
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
        let frac = s0 as f64 - start as f64 * ratio;
        for k in 0..frames {
            let pos = k as f64 * ratio - frac;
            let i0 = pos.floor().max(0.0) as usize;
            let f = (pos - i0 as f64) as f32;
            for c in 0..ch {
                let a = src[c].get(i0).copied().unwrap_or(0.0);
                let b = src[c].get(i0 + 1).copied().unwrap_or(a);
                out.channels[c][k] = a + (b - a) * f;
            }
        }
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
