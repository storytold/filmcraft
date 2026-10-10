//! AVI media source (`filmcraft-avi` demux, GOP-aware video via [`crate::gop`]).
//!
//! Video: Motion JPEG (frames without their own Huffman tables get the standard ones), H.264 and
//! HEVC (Annex B byte streams, our decoders), and uncompressed RGB / YUV ([`RawFormat`]). A frame
//! slot with an empty chunk (a "drop frame") repeats the picture before it. MPEG-4 Part 2
//! (DivX / Xvid) and DV have no decoder yet: their streams are listed and report the codec.
//!
//! Audio: PCM (integer and float) straight from the chunks; MPEG audio (MP3, MP2) and AC-3 frames,
//! which AVI chunks need not follow, are found again across chunk boundaries
//! ([`filmcraft_mpegts::frame_bytes`]) and decoded as packets.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filmcraft_avi::{AviFile, BitmapInfo, StreamKind, WaveFormat};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_isobmff::{PcmConfig, SampleEntry};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::audio::{PacketDecoder, decode_pcm};
use crate::gop::{GopCache, VideoSamples};
use crate::video::{RawFormat, RawVideoDecoder, VideoDecoder};
use crate::{CodecError, make_video_decoder};

pub fn sniff(b: &[u8]) -> bool {
    filmcraft_avi::sniff(b)
}

#[derive(Clone, Debug, PartialEq)]
enum VCodec {
    Jpeg,
    H264,
    Hevc,
    Raw(RawFormat),
    /// No decoder: the codec's name.
    Unsupported(String),
}

fn fourcc_text(f: [u8; 4]) -> String {
    let t: String = f.iter().map(|&c| if c.is_ascii_graphic() { c as char } else { '?' }).collect();
    t.trim_end_matches('?').to_string()
}

/// The decoder for a video stream and its label.
fn video_codec(b: &BitmapInfo) -> (VCodec, String) {
    if let Some(raw) = RawFormat::from_avi(b.compression, b.bit_count) {
        return (VCodec::Raw(raw), raw.label().into());
    }
    let mut f = b.compression;
    f.make_ascii_uppercase();
    match &f {
        b"MJPG" | b"AVRN" | b"LJPG" | b"JPGL" | b"DMB1" | b"JPEG" | b"MJPA" => (VCodec::Jpeg, "Motion JPEG".into()),
        b"H264" | b"X264" | b"AVC1" | b"DAVC" | b"VSSH" => (VCodec::H264, "H.264".into()),
        b"HEVC" | b"H265" | b"HEV1" | b"HVC1" | b"X265" => (VCodec::Hevc, "HEVC".into()),
        b"DIVX" | b"DX50" | b"XVID" | b"FMP4" | b"MP4V" | b"M4S2" | b"3IV2" | b"DIV3" | b"MP43" | b"MP42" => {
            (VCodec::Unsupported("MPEG-4 Part 2 (DivX / Xvid)".into()), "MPEG-4 Part 2 (DivX / Xvid)".into())
        }
        b"DVSD" | b"DV25" | b"DV50" | b"DVHD" | b"DVSL" | b"CDVC" | b"DVCP" | b"DVPP" => (VCodec::Unsupported("DV".into()), "DV".into()),
        _ => {
            let name = fourcc_text(b.compression);
            (VCodec::Unsupported(format!("\"{name}\"")), name)
        }
    }
}

/// One picture of the video stream: its frame slot (in `scale / rate` units) and chunk.
#[derive(Clone, Copy, Debug)]
struct VFrame {
    pts: i64,
    chunk: usize,
    key: bool,
}

#[derive(Clone, Debug)]
enum ACodec {
    Pcm(PcmConfig),
    /// MPEG audio (layer 1-3) or AC-3 frames, found again across chunks.
    Framed {
        ts: filmcraft_mpegts::Codec,
        layer: u8,
    },
    Unsupported(String),
}

fn audio_codec(w: &WaveFormat) -> (ACodec, String) {
    let pcm = |float: bool| {
        let bits = w.bits_per_sample;
        let cfg = PcmConfig { bits, float, big_endian: false, signed: bits > 8, channels: u32::from(w.channels.max(1)), sample_rate: f64::from(w.sample_rate) };
        (ACodec::Pcm(cfg), format!("PCM {bits}-bit{}", if float { " float" } else { "" }))
    };
    match w.format_tag {
        1 if matches!(w.bits_per_sample, 8 | 16 | 24 | 32) => pcm(false),
        3 if matches!(w.bits_per_sample, 32 | 64) => pcm(true),
        0x55 => (ACodec::Framed { ts: filmcraft_mpegts::Codec::MpegAudio, layer: 3 }, "MP3".into()),
        0x50 => (ACodec::Framed { ts: filmcraft_mpegts::Codec::MpegAudio, layer: 2 }, "MPEG Audio".into()),
        0x2000 => (ACodec::Framed { ts: filmcraft_mpegts::Codec::Ac3, layer: 0 }, "AC-3".into()),
        tag => {
            let name = format!("audio format 0x{tag:04X}");
            (ACodec::Unsupported(name.clone()), name)
        }
    }
}

/// A slice of a chunk: (chunk, offset in it, length).
type Piece = (usize, usize, usize);

struct AudioState {
    decoder: Option<PacketDecoder>,
    packets: HashMap<usize, Arc<Vec<Vec<f32>>>>,
    order: Vec<usize>,
    last_decoded: Option<usize>,
}

struct AviAudio {
    stream: usize,
    codec: ACodec,
    /// The pieces of packet `k` are `pieces[first[k]..first[k + 1]]`.
    pieces: Vec<Piece>,
    first: Vec<usize>,
    /// Packet start positions in sample frames.
    starts: Vec<i64>,
    state: Mutex<AudioState>,
}

pub struct AviSource {
    info: MediaInfo,
    bytes: crate::Src,
    file: AviFile,
    vstream: Option<usize>,
    vcodec: VCodec,
    /// Pictures in stored (decode) order.
    vframes: Vec<VFrame>,
    /// `vframes` indices in presentation order (H.264 with B-frames is stored in decode order),
    /// and their presentation times.
    vorder: Vec<usize>,
    vorder_pts: Vec<i64>,
    /// Frame slots of the video stream (where the last picture ends).
    vslots: i64,
    rate: FrameRate,
    bottom_up: bool,
    video: GopCache,
    audios: Vec<AviAudio>,
}

/// Whether an Annex B access unit holds an H.264 IDR or HEVC IRAP picture.
fn random_access(data: &[u8], hevc: bool) -> bool {
    filmcraft_bitstream::annexb_nals(data).iter().any(|n| match n.first() {
        Some(h) if hevc => (16..=23).contains(&((h >> 1) & 0x3F)),
        Some(h) => h & 0x1F == 5,
        None => false,
    })
}

/// The packets of a framed audio stream: every frame header found in the stream of chunks, and
/// the samples each frame decodes to. Bytes that start no frame are skipped (resynchronisation).
fn split_frames(
    file: &AviFile,
    bytes: &crate::Src,
    s: usize,
    ts: &filmcraft_mpegts::Codec,
) -> (Vec<Piece>, Vec<usize>, Vec<u32>, Option<filmcraft_mpegts::FrameInfo>) {
    let Some(st) = file.streams.get(s) else { return Default::default() };
    // chunk start positions in the concatenated stream
    let mut at = Vec::with_capacity(st.chunks.len());
    let mut total = 0usize;
    for c in &st.chunks {
        at.push(total);
        total = total.saturating_add(c.size as usize);
    }
    let locate = |pos: usize| -> Option<(usize, usize)> {
        let k = at.partition_point(|&a| a <= pos).checked_sub(1)?;
        Some((k, pos - at.get(k)?))
    };
    let (mut pieces, mut first, mut samples) = (Vec::new(), Vec::new(), Vec::new());
    let mut info = None;
    // a sliding window over the chunks: `win` holds the stream from `base`
    let (mut win, mut base, mut next) = (Vec::<u8>::new(), 0usize, 0usize);
    let mut pos = 0usize;
    while pos < total {
        // keep at least a header's worth (and the frame, when we know its length) in the window
        while win.len() < (pos - base) + 4096 && next < st.chunks.len() {
            win.extend(file.read_chunk(&bytes, s, next).unwrap_or_default());
            next += 1;
        }
        let Some(here) = win.get(pos - base..) else { break };
        match filmcraft_mpegts::frame_bytes(ts, here) {
            Some(n) if pos + n <= total => {
                if info.is_none() {
                    info = filmcraft_mpegts::frame_info(ts, here);
                }
                let k = filmcraft_mpegts::frame_info(ts, here).map_or(0, |f| f.samples);
                first.push(pieces.len());
                let mut p = pos;
                while p < pos + n {
                    let Some((c, off)) = locate(p) else { break };
                    let len = (st.chunks.get(c).map_or(0, |ch| ch.size as usize) - off).min(pos + n - p);
                    if len == 0 {
                        break;
                    }
                    pieces.push((c, off, len));
                    p += len;
                }
                samples.push(k);
                pos += n;
            }
            _ if here.len() < 8 && next >= st.chunks.len() => break,
            _ => pos += 1,
        }
        // drop what is behind
        if pos - base > 1 << 16 {
            let cut = pos - base;
            win.drain(..cut.min(win.len()));
            base += cut;
        }
    }
    (pieces, first, samples, info)
}

impl AviSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    /// Open from a random-access reader: the index now, frames and audio on demand.
    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bytes = crate::Src(reader);
        let file = filmcraft_avi::open(&bytes).map_err(|e| CodecError::Container(e.to_string()))?;
        let vstream = file.streams.iter().position(|s| s.kind == StreamKind::Video && s.video.is_some() && !s.chunks.is_empty());
        let astreams: Vec<usize> = file
            .streams
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == StreamKind::Audio && s.audio.is_some() && !s.chunks.is_empty())
            .map(|(i, _)| i)
            .take(filmcraft_media::MAX_AUDIO_STREAMS)
            .collect();
        if vstream.is_none() && astreams.is_empty() {
            return Err(CodecError::Unsupported("no playable streams".into()));
        }
        // video: frame slots, pictures and key frames
        let (mut vcodec, mut vframes, mut vslots, mut rate, mut bottom_up, mut video) =
            (VCodec::Unsupported(String::new()), Vec::new(), 0i64, FrameRate::new(25, 1), false, None);
        if let Some(vs) = vstream.and_then(|i| file.streams.get(i)) {
            let b = vs.video.clone().unwrap_or_default();
            let (codec, label) = video_codec(&b);
            rate = FrameRate::new(i64::from(vs.rate), i64::from(vs.scale));
            bottom_up = b.height > 0;
            let (w, h) = (b.width.unsigned_abs(), b.height.unsigned_abs());
            for (k, c) in vs.chunks.iter().enumerate() {
                if c.size == 0 {
                    continue;
                }
                let key = match &codec {
                    VCodec::H264 | VCodec::Hevc if !vs.keyframes_known => {
                        file.read_chunk(&bytes, vstream.unwrap_or(0), k).is_ok_and(|d| random_access(&d, codec == VCodec::Hevc))
                    }
                    VCodec::H264 | VCodec::Hevc => c.key,
                    _ => true,
                };
                vframes.push(VFrame { pts: i64::from(vs.start) + k as i64, chunk: k, key });
            }
            vslots = i64::from(vs.start) + vs.chunks.len() as i64;
            // AVI has no timestamps: H.264 with B-frames is stored in decode order, and the
            // presentation order comes from the pictures' order counts. A first look at the start
            // of the stream decides whether it reorders at all (reading every frame's header is the
            // cost of a reordering stream only).
            if codec == VCodec::H264
                && let Some(si) = vstream
            {
                let heads = |n: usize| -> Vec<Vec<u8>> {
                    vframes
                        .iter()
                        .take(n)
                        .map(|f| {
                            file.streams
                                .get(si)
                                .and_then(|s| s.chunks.get(f.chunk))
                                .and_then(|c| filmcraft_media::reader::read_range(&*bytes.0, c.offset, (c.size as usize).min(8 << 10)).ok())
                                .unwrap_or_default()
                        })
                        .collect()
                };
                let reorders = |p: &[i64]| p.windows(2).any(|w| w[1] < w[0]);
                if crate::mxf::avc_presentation_order(&heads(300)).is_some_and(|p| reorders(&p))
                    && let Some(ranks) = crate::mxf::avc_presentation_order(&heads(vframes.len()))
                {
                    // the pictures keep the slots they occupy (empty drop-frame chunks between them
                    // stay), shown in their presentation order
                    let mut slots: Vec<i64> = vframes.iter().map(|f| f.pts).collect();
                    slots.sort_unstable();
                    for (f, r) in vframes.iter_mut().zip(ranks) {
                        if let Some(&p) = usize::try_from(r).ok().and_then(|r| slots.get(r)) {
                            f.pts = p;
                        }
                    }
                }
            }
            let secs = vslots as f64 / rate.as_f64().max(1e-9);
            // The rate the pictures play at: a writer that kept a fine time base (ffmpeg copying an
            // MP4's 1/600) fills the slots between pictures with empty chunks, every 20th slot
            // holding a picture at 30 fps. A regular spacing makes the rate; slots stay the units.
            let shown = if vframes.len() > 1 {
                let mut gaps: Vec<i64> = vframes.windows(2).map(|w| w[1].chunk as i64 - w[0].chunk as i64).filter(|g| *g > 0).take(1000).collect();
                gaps.sort_unstable();
                let gap = gaps.get(gaps.len() / 2).copied().unwrap_or(1).max(1);
                let regular = gaps.iter().filter(|g| **g == gap).count() * 10 >= gaps.len() * 9;
                if gap > 1 && regular { FrameRate::new(i64::from(vs.rate), i64::from(vs.scale).saturating_mul(gap)) } else { rate }
            } else {
                rate
            };
            let bitrate = (secs > 0.0).then(|| (vs.chunks.iter().map(|c| u64::from(c.size)).sum::<u64>() as f64 * 8.0 / secs) as u64);
            video = Some(VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: shown,
                par: (1, 1),
                codec: label,
                pixel_format: String::new(),
                color: crate::stream_color::resolve(w, h, &[]),
                has_alpha: false,
                bitrate,
                hdr: None,
            });
            vcodec = codec;
        }
        // audio: packets and their start positions
        let mut audios = Vec::new();
        let mut audio_streams = Vec::new();
        for &s in &astreams {
            let Some(st) = file.streams.get(s) else { continue };
            let w = st.audio.clone().unwrap_or_default();
            let (codec, label) = audio_codec(&w);
            let mut sample_rate = w.sample_rate.max(1);
            let mut channels = u32::from(w.channels.max(1));
            let (pieces, first, starts) = match &codec {
                ACodec::Pcm(cfg) => {
                    let block = usize::from(w.block_align.max(1)).max((usize::from(cfg.bits).div_ceil(8)) * channels as usize);
                    let mut t = 0i64;
                    let mut starts = Vec::with_capacity(st.chunks.len());
                    for c in &st.chunks {
                        starts.push(t);
                        t = t.saturating_add((c.size as usize / block) as i64);
                    }
                    let pieces: Vec<Piece> = st.chunks.iter().enumerate().map(|(k, c)| (k, 0, c.size as usize)).collect();
                    let first = (0..pieces.len()).collect();
                    (pieces, first, starts)
                }
                ACodec::Framed { ts, .. } => {
                    let (pieces, first, samples, info) = split_frames(&file, &bytes, s, ts);
                    if let Some(i) = info {
                        sample_rate = i.sample_rate.max(1);
                        channels = i.channels.max(1);
                    }
                    let mut t = 0i64;
                    let starts = samples
                        .iter()
                        .map(|&k| {
                            let s = t;
                            t = t.saturating_add(i64::from(k));
                            s
                        })
                        .collect();
                    (pieces, first, starts)
                }
                ACodec::Unsupported(_) => (Vec::new(), Vec::new(), Vec::new()),
            };
            // a stream that starts late (dwStart, in scale / rate units)
            let delay = (i128::from(st.start) * i128::from(st.scale) * i128::from(sample_rate) / i128::from(st.rate.max(1))) as i64;
            let starts: Vec<i64> = starts.into_iter().map(|s: i64| s.saturating_add(delay)).collect();
            audio_streams.push(AudioStreamInfo {
                sample_rate,
                channels,
                codec: label,
                bits_per_sample: (w.bits_per_sample > 0).then_some(u32::from(w.bits_per_sample)),
            });
            let state = Mutex::new(AudioState { decoder: None, packets: HashMap::new(), order: Vec::new(), last_decoded: None });
            audios.push(AviAudio { stream: s, codec, pieces, first, starts, state });
        }
        // the longer of the picture and the sound
        let vdur = if vslots > 0 { rate.tick_of(vslots) } else { Tick::ZERO };
        let adur = audios
            .iter()
            .zip(&audio_streams)
            .filter_map(|(a, i)| {
                let last = a.starts.last()?;
                let tail = a.starts.len().checked_sub(2).and_then(|k| a.starts.get(k)).map_or(0, |p| last - p);
                Some(Tick::from_rational(last + tail, 1, i64::from(i.sample_rate)))
            })
            .max()
            .unwrap_or_default();
        let info = MediaInfo {
            name: name.to_string(),
            kind: if video.is_some() { MediaKind::Movie } else { MediaKind::AudioOnly },
            duration: vdur.max(adur),
            video,
            audio_streams,
            container: "AVI".into(),
            start_timecode: None,
            file_size: Some(bytes.0.len()),
        };
        let mut vorder: Vec<usize> = (0..vframes.len()).collect();
        vorder.sort_by_key(|&i| vframes.get(i).map_or(0, |f| f.pts));
        let vorder_pts = vorder.iter().map(|&i| vframes.get(i).map_or(0, |f| f.pts)).collect();
        Ok(Self { info, bytes, file, vstream, vcodec, vframes, vorder, vorder_pts, vslots, rate, bottom_up, video: GopCache::new(None), audios })
    }

    fn read(&self, stream: usize, chunk: usize) -> crate::Result<Vec<u8>> {
        self.file.read_chunk(&self.bytes, stream, chunk).map_err(|e| CodecError::Container(e.to_string()))
    }

    /// The bytes of audio packet `k`.
    fn packet(&self, a: &AviAudio, k: usize) -> crate::Result<Vec<u8>> {
        let (from, to) = (
            a.first.get(k).copied().ok_or_else(|| CodecError::Container("audio packet out of range".into()))?,
            a.first.get(k + 1).copied().unwrap_or(a.pieces.len()),
        );
        let mut out = Vec::new();
        for &(c, off, len) in a.pieces.get(from..to).unwrap_or_default() {
            let chunk = self.read(a.stream, c)?;
            out.extend_from_slice(chunk.get(off..off + len).ok_or_else(|| CodecError::Container("audio chunk shorter than its index".into()))?);
        }
        Ok(out)
    }

    fn audio_packet(&self, a: &AviAudio, st: &mut AudioState, ainfo: &AudioStreamInfo, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
        if let Some(p) = st.packets.get(&i) {
            return Ok(p.clone());
        }
        let data = self.packet(a, i)?;
        let decoded = match &a.codec {
            ACodec::Pcm(cfg) => decode_pcm(&data, cfg),
            ACodec::Unsupported(name) => return Err(CodecError::Unsupported(name.clone())),
            ACodec::Framed { ts, layer } => {
                if st.decoder.is_none() {
                    st.decoder = Some(match ts {
                        filmcraft_mpegts::Codec::Ac3 => PacketDecoder::ac3()?,
                        _ => PacketDecoder::mpeg_audio(*layer, ainfo.sample_rate)?,
                    });
                }
                let d = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?;
                // non-sequential access: reset and prime with the packet before (the bit reservoir)
                if st.last_decoded.is_none_or(|l| l + 1 != i) {
                    d.reset();
                    if let Some(prev) = i.checked_sub(1).and_then(|j| self.packet(a, j).ok()) {
                        let _ = d.decode(&prev, 0);
                    }
                }
                let start = a.starts.get(i).copied().unwrap_or(0).max(0) as u64;
                let r = d.decode(&data, start);
                st.last_decoded = Some(i);
                r.unwrap_or_default()
            }
        };
        let p = Arc::new(decoded);
        st.packets.insert(i, p.clone());
        st.order.push(i);
        if st.order.len() > 4096 {
            let old = st.order.remove(0);
            st.packets.remove(&old);
        }
        Ok(p)
    }
}

/// The video stream as a [`VideoSamples`] table.
struct AviVideo<'a> {
    src: &'a AviSource,
    stream: usize,
}

impl VideoSamples for AviVideo<'_> {
    fn count(&self) -> usize {
        self.src.vframes.len()
    }
    fn pts(&self, i: usize) -> i64 {
        self.src.vframes.get(i).map_or(0, |f| f.pts)
    }
    fn sync_before(&self, i: usize) -> usize {
        let i = i.min(self.src.vframes.len().saturating_sub(1));
        self.src.vframes.get(..=i).and_then(|f| f.iter().rposition(|f| f.key)).unwrap_or(0)
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        // the picture shown in slot `t`: the last one at or before it (empty chunks repeat it)
        if t >= self.src.vslots {
            return None;
        }
        let k = self.src.vorder_pts.partition_point(|&p| p <= t).checked_sub(1)?;
        self.src.vorder.get(k).copied()
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        let f = self.src.vframes.get(i).ok_or_else(|| CodecError::Container("frame out of range".into()))?;
        self.src.read(self.stream, f.chunk)
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        let v = self.src.info.video.as_ref().ok_or_else(|| CodecError::Unsupported("no video stream".into()))?;
        let (w, h) = (v.width.min(u32::from(u16::MAX)) as u16, v.height.min(u32::from(u16::MAX)) as u16);
        match &self.src.vcodec {
            VCodec::Jpeg => make_video_decoder(&SampleEntry::jpeg(w, h)),
            VCodec::H264 => Ok(Box::new(crate::video::H264Decoder::annexb())),
            VCodec::Hevc => Ok(Box::new(crate::video::HevcDecoder::annexb())),
            VCodec::Raw(f) => Ok(Box::new(RawVideoDecoder::new(*f, v.width, v.height, self.src.bottom_up)?)),
            VCodec::Unsupported(name) => Err(CodecError::Unsupported(format!("no decoder for {name} video"))),
        }
    }
}

impl MediaSource for AviSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let s = self.vstream.ok_or(MediaError::NoStream("video"))?;
        let t = req.time.max(Tick::ZERO);
        let slot = |t: Tick| self.rate.frame_at(t);
        let late = filmcraft_media::cancel::catch_up().map(|m| slot(t - m));
        Ok(self.video.frame_late(&AviVideo { src: self, stream: s }, slot(t), late)?)
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.audio_stream(0, start, frames, sample_rate)
    }

    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let a = self.audios.get(stream).ok_or(MediaError::NoStream("audio"))?;
        let ainfo = self.info.audio_streams.get(stream).ok_or(MediaError::NoStream("audio"))?;
        if let ACodec::Unsupported(name) = &a.codec {
            return Err(MediaError::Unsupported(format!("{name} audio")));
        }
        let ch = ainfo.channels.max(1) as usize;
        let ratio = f64::from(ainfo.sample_rate) / f64::from(sample_rate.max(1));
        let s0 = (start as f64 * ratio).floor() as i64;
        let need = (frames as f64 * ratio).ceil() as i64 + 2;
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        let mut st = a.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut i = a.starts.partition_point(|&x| x <= s0).saturating_sub(1);
        while let Some(&pk_start) = a.starts.get(i) {
            if pk_start >= s0 + need {
                break;
            }
            let pk = self.audio_packet(a, &mut st, ainfo, i)?;
            for (c, dst) in src.iter_mut().enumerate() {
                let Some(chan) = pk.get(c.min(pk.len().saturating_sub(1))) else { continue };
                for (k, v) in chan.iter().enumerate() {
                    let pos = pk_start + k as i64 - s0;
                    if let Some(d) = usize::try_from(pos).ok().and_then(|p| dst.get_mut(p)) {
                        *d = *v;
                    }
                }
            }
            i += 1;
        }
        drop(st);
        let frac = s0 as f64 - start as f64 * ratio;
        let mut out = AudioBuffer::silence(sample_rate, ch, frames);
        for (c, o) in out.channels.iter_mut().enumerate() {
            let Some(s) = src.get(c) else { continue };
            for (k, d) in o.iter_mut().enumerate() {
                let pos = k as f64 * ratio - frac;
                let i0 = pos.floor().max(0.0) as usize;
                let f = (pos - i0 as f64) as f32;
                let a = s.get(i0).copied().unwrap_or(0.0);
                let b = s.get(i0 + 1).copied().unwrap_or(a);
                *d = a + (b - a) * f;
            }
        }
        Ok(out)
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes) {
        return None;
    }
    Some(AviSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

/// [`filmcraft_media::ReaderOpener`] for AVI.
pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(head) {
        return None;
    }
    Some(AviSource::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}
