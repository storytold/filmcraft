//! MPEG-2 transport streams (`.ts`, `.m2ts`, `.mts`), program streams (`.mpg`, `.vob`, `.mod`)
//! and MPEG-1/2 video elementary streams (`.m2v`, `.m1v`).
//!
//! - Demux and the access-unit index come from `filmcraft-mpegts` (built when the file opens).
//! - Video: MPEG-1/2 (our decoder), H.264 and HEVC (Annex B byte streams, our decoders) with
//!   GOP-aware random access ([`crate::gop`]). Frames are presented in display order, from the
//!   first random-access picture; display order comes from the PTS (or, without timestamps, the
//!   GOP and temporal reference). Leading pictures of an open GOP decode from the previous GOP.
//! - Audio: MPEG audio layers I-III (symphonia), AAC in ADTS or LATM (our decoder), AC-3 (our
//!   decoder), Blu-ray / AVCHD and DVD LPCM. Audio frames are placed on the timeline from their
//!   PTS relative to the first video frame, sample-contiguously (re-anchored only at gaps).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_mpegts::{Codec, File, Kind, Unit};
use filmcraft_time::{FrameRate, Tick};

use crate::CodecError;
use crate::gop::{GopCache, VideoSamples};
use crate::video::{H264Decoder, HevcDecoder, Mpeg2Decoder, VideoDecoder};

impl filmcraft_mpegts::ByteSource for crate::Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

/// An MPEG-1/2 video elementary stream starts with a sequence header.
fn is_video_es(head: &[u8]) -> bool {
    head.starts_with(&[0, 0, 1, 0xB3])
}

pub fn sniff(head: &[u8]) -> bool {
    filmcraft_mpegts::sniff(head).is_some() || is_video_es(head)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VKind {
    Mpeg2,
    H264,
    Hevc,
}

/// The presented video: samples in decode order from the first random-access unit.
struct VideoTrack {
    kind: VKind,
    /// Stream index in the demuxed file (unused for elementary streams).
    stream: usize,
    /// Unit indices (into the stream's units, or the elementary stream's access units).
    units: Vec<usize>,
    keys: Vec<bool>,
    /// Display position of each sample (negative: not presented).
    pts: Vec<i64>,
    /// Display position → sample.
    order: Vec<usize>,
    /// MPEG-2 sequence header for priming decoders.
    header: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
enum ACodec {
    Mpa(u8),
    Adts,
    Latm,
    Ac3,
    LpcmBluray,
    LpcmDvd(filmcraft_mpegts::LpcmFormat),
}

struct AudioState {
    dec: Option<AudioDecoder>,
    cache: HashMap<usize, Arc<Vec<Vec<f32>>>>,
    order: VecDeque<usize>,
    last: Option<usize>,
}

struct AudioTrack {
    stream: usize,
    codec: ACodec,
    rate: u32,
    channels: usize,
    /// Presentation sample (at `rate`, relative to the first video frame) of each unit.
    starts: Vec<i64>,
    /// Samples in each unit.
    lens: Vec<u32>,
    /// LATM: the stream's configuration (frames may only reference it).
    latm: Option<crate::audio::LatmConfig>,
    /// DVD LPCM: byte offset of each unit in the elementary stream (and the end).
    byte_starts: Vec<u64>,
    state: Mutex<AudioState>,
}

pub struct MpegSource {
    info: MediaInfo,
    bytes: crate::Src,
    file: Option<File>,
    /// Elementary stream: the whole file and its access units.
    es: Option<(Arc<[u8]>, Vec<std::ops::Range<usize>>)>,
    video: Option<VideoTrack>,
    gop: GopCache,
    audio: Option<AudioTrack>,
    /// Why the video or audio stream cannot be decoded.
    unsupported_video: Option<String>,
    unsupported_audio: Option<String>,
}

fn vkind(c: &Codec) -> Option<VKind> {
    match c {
        Codec::Mpeg1Video | Codec::Mpeg2Video => Some(VKind::Mpeg2),
        Codec::H264 => Some(VKind::H264),
        Codec::Hevc => Some(VKind::Hevc),
        _ => None,
    }
}

fn audio_supported(c: &Codec) -> bool {
    matches!(c, Codec::MpegAudio | Codec::AacAdts | Codec::AacLatm | Codec::LpcmBluray | Codec::LpcmDvd | Codec::Ac3)
}

/// The coded rate unless the timestamps say otherwise (3:2 pulldown, field-rate coding). Never
/// zero: damaged timestamps spread over a huge span give a rate that rounds to 0 fps, which the
/// duration computation divided by.
fn stream_rate(coded: Option<FrameRate>, from_pts: Option<f64>) -> FrameRate {
    let rate = match (coded, from_pts) {
        (Some(r), Some(p)) if (r.as_f64() - p).abs() / p > 0.01 => FrameRate::from_f64(p),
        (Some(r), _) => r,
        (None, Some(p)) => FrameRate::from_f64(p),
        (None, None) => FrameRate::FPS_25,
    };
    if rate.num > 0 && rate.den > 0 { rate } else { FrameRate::FPS_25 }
}

/// Display-order keys of samples (decode order): the PTS when every unit has one, else (MPEG
/// video) GOP start + temporal_reference, else the decode order.
fn display_keys(units: &[&Unit], mpeg: bool) -> Vec<i64> {
    if units.iter().all(|u| u.pts.is_some()) {
        return units.iter().map(|u| u.pts.unwrap_or(0)).collect();
    }
    if mpeg {
        let mut out = Vec::with_capacity(units.len());
        let mut base = 0i64;
        for (i, u) in units.iter().enumerate() {
            if let Some(p) = u.picture
                && (p.gop || i == 0)
            {
                base = i as i64;
            }
            out.push(base + u.picture.map_or(0, |p| p.temporal_reference as i64));
        }
        // scale so that a later GOP never sorts before an earlier one
        return out.iter().map(|&k| k * 4).collect();
    }
    let mut last = i64::MIN / 4;
    units
        .iter()
        .map(|u| {
            last = u.pts.unwrap_or(last + 1);
            last
        })
        .collect()
}

/// Presentation order of `keys` (decode order) from sample 0 (a random-access unit): samples
/// shown before it (leading pictures without their references) are not presented.
fn presentation(keys: &[i64]) -> (Vec<i64>, Vec<usize>) {
    let mut idx: Vec<usize> = (0..keys.len()).filter(|&i| keys[i] >= keys[0]).collect();
    idx.sort_by_key(|&i| (keys[i], i));
    let mut pts = vec![0i64; keys.len()];
    for (i, p) in pts.iter_mut().enumerate() {
        *p = -(i as i64) - 1;
    }
    for (d, &i) in idx.iter().enumerate() {
        pts[i] = d as i64;
    }
    (pts, idx)
}

impl MpegSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    /// Open from a random-access reader: the whole file is scanned once to index its access units.
    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bytes = crate::Src(reader);
        let head = filmcraft_media::reader::read_range(&*bytes.0, 0, 4096).map_err(|e| CodecError::Container(e.to_string()))?;
        if is_video_es(&head) {
            return Self::open_es(name, bytes);
        }
        let file = filmcraft_mpegts::open(&bytes).map_err(|e| match e {
            filmcraft_mpegts::Error::NotMpeg => CodecError::Unsupported("not an MPEG stream".into()),
            other => CodecError::Container(other.to_string()),
        })?;
        for w in &file.warnings {
            log::warn!("{name}: {w}");
        }
        let vstream = file.find(Kind::Video, |c| vkind(c).is_some()).or_else(|| file.find(Kind::Video, |_| true));
        let astream = file.find(Kind::Audio, audio_supported).or_else(|| file.find(Kind::Audio, |_| true));
        if vstream.is_none() && astream.is_none() {
            return Err(CodecError::Unsupported(format!("{}: no video or audio streams", file.format.name())));
        }
        let mut src = MpegSource {
            info: MediaInfo {
                name: name.to_string(),
                kind: MediaKind::Movie,
                duration: Tick::ZERO,
                video: None,
                audio_streams: Vec::new(),
                container: file.format.name().to_string(),
                start_timecode: None,
                file_size: Some(bytes.0.len()),
            },
            bytes,
            file: None,
            es: None,
            video: None,
            gop: GopCache::new(None),
            audio: None,
            unsupported_video: None,
            unsupported_audio: None,
        };
        // video
        let origin;
        if let Some(vi) = vstream {
            let st = &file.streams[vi];
            match vkind(&st.codec) {
                Some(kind) => {
                    let first_key =
                        st.units.iter().position(|u| u.key).ok_or_else(|| CodecError::Decode("no random-access picture in the video stream".into()))?;
                    let sel: Vec<&Unit> = st.units[first_key..].iter().collect();
                    let keys = display_keys(&sel, kind == VKind::Mpeg2);
                    let (pts, order) = presentation(&keys);
                    // timeline origin: the PTS of the first presented picture
                    origin = order.first().and_then(|&i| sel[i].pts).or_else(|| sel.iter().filter_map(|u| u.pts).min());
                    let rate_from_pts = {
                        let shown: Vec<i64> = order.iter().filter_map(|&i| sel[i].pts).collect();
                        (shown.len() >= 2 && shown.len() == order.len())
                            .then(|| (shown.len() - 1) as f64 * 90_000.0 / (shown[shown.len() - 1] - shown[0]).max(1) as f64)
                    };
                    let track = VideoTrack {
                        kind,
                        stream: vi,
                        units: (first_key..st.units.len()).collect(),
                        keys: sel.iter().map(|u| u.key).collect(),
                        pts,
                        order,
                        header: Vec::new(),
                    };
                    src.file = Some(file);
                    src.video = Some(track);
                    src.describe_video(rate_from_pts)?;
                    if let Some(file) = src.file.take() {
                        src.setup_audio(file, astream, origin);
                    }
                }
                None => {
                    src.unsupported_video = Some(format!("{} video in {} (FilmCraft has no decoder for it)", st.codec.name(), file.format.name()));
                    src.info.video = Some(VideoStreamInfo {
                        width: 0,
                        height: 0,
                        frame_rate: FrameRate::FPS_25,
                        par: (1, 1),
                        codec: format!("{} (unsupported)", st.codec.name()),
                        pixel_format: String::new(),
                        color: filmcraft_color::ColorInfo::REC709,
                        has_alpha: false,
                        bitrate: None,
                        hdr: None,
                    });
                    src.setup_audio(file, astream, None);
                }
            }
        } else {
            src.info.kind = MediaKind::AudioOnly;
            src.setup_audio(file, astream, None);
        }
        Ok(src)
    }

    /// A raw MPEG-1/2 video elementary stream (no timestamps: GOP / temporal reference order).
    fn open_es(name: &str, bytes: crate::Src) -> crate::Result<Self> {
        let all: Arc<[u8]> = filmcraft_media::reader::read_range(&*bytes.0, 0, usize::try_from(bytes.0.len()).unwrap_or(usize::MAX))
            .map_err(|e| CodecError::Container(e.to_string()))?
            .into();
        let aus = filmcraft_mpeg2v::access_units(&all);
        let infos: Vec<filmcraft_mpeg2v::AccessUnitInfo> = aus.iter().map(|r| filmcraft_mpeg2v::scan_access_unit(&all[r.clone()])).collect();
        let first_key = infos.iter().position(|a| a.is_intra()).ok_or_else(|| CodecError::Decode("no I picture in the stream".into()))?;
        // fake units carrying the picture info for display ordering
        let mut keys = Vec::new();
        let mut base = 0i64;
        for (k, a) in infos[first_key..].iter().enumerate() {
            if a.gop.is_some() || k == 0 {
                base = k as i64;
            }
            keys.push((base + a.pictures.first().map_or(0, |p| p.0.temporal_reference as i64)) * 4);
        }
        let (pts, order) = presentation(&keys);
        let track = VideoTrack {
            kind: VKind::Mpeg2,
            stream: 0,
            units: (first_key..aus.len()).collect(),
            keys: infos[first_key..].iter().map(|a| a.is_intra()).collect(),
            pts,
            order,
            header: Vec::new(),
        };
        let mut src = MpegSource {
            info: MediaInfo {
                name: name.to_string(),
                kind: MediaKind::Movie,
                duration: Tick::ZERO,
                video: None,
                audio_streams: Vec::new(),
                container: "MPEG video elementary stream".into(),
                start_timecode: None,
                file_size: Some(bytes.0.len()),
            },
            bytes,
            file: None,
            es: Some((all, aus)),
            video: Some(track),
            gop: GopCache::new(None),
            audio: None,
            unsupported_video: None,
            unsupported_audio: None,
        };
        src.describe_video(None)?;
        // the first GOP's time code (non-drop-frame count at the nominal rate)
        if let Some(tc) = infos[first_key].gop {
            let fps = src.info.frame_rate().as_f64().round().max(1.0) as i64;
            src.info.start_timecode = Some(((tc.hours as i64 * 60 + tc.minutes as i64) * 60 + tc.seconds as i64) * fps + tc.pictures as i64);
        }
        Ok(src)
    }

    /// The demuxed file (streams and their access-unit tables); `None` for elementary streams.
    pub fn file(&self) -> Option<&File> {
        self.file.as_ref()
    }

    /// Timeline position (samples at the audio stream's rate) of the first audio frame, and the
    /// number of samples in the audio stream.
    pub fn audio_extent(&self) -> Option<(i64, i64)> {
        let a = self.audio.as_ref()?;
        let first = *a.starts.first()?;
        let end = a.starts.last()? + *a.lens.last()? as i64;
        Some((first, end - first))
    }

    /// The demuxed stream indices of the presented video and audio.
    pub fn stream_indices(&self) -> (Option<usize>, Option<usize>) {
        (self.video.as_ref().filter(|_| self.es.is_none()).map(|v| v.stream), self.audio.as_ref().map(|a| a.stream))
    }

    /// Presentation timestamps (90 kHz) of the presented video frames in display order. Frames
    /// whose PES packet carried none are interpolated at the frame duration from the nearest
    /// frame with one (`None` only when no frame has a timestamp, and for elementary streams).
    pub fn video_pts(&self) -> Vec<Option<i64>> {
        let (Some(v), Some(f)) = (&self.video, &self.file) else { return Vec::new() };
        let units = &f.streams[v.stream].units;
        let raw: Vec<Option<i64>> = v.order.iter().map(|&i| units[v.units[i]].pts).collect();
        let rate = self.info.frame_rate();
        let dur = |k: i64| (k as i128 * 90_000 * rate.den as i128 / rate.num.max(1) as i128) as i64;
        let known: Vec<usize> = (0..raw.len()).filter(|&i| raw[i].is_some()).collect();
        (0..raw.len())
            .map(|i| {
                raw[i].or_else(|| {
                    let j = known.iter().rev().find(|&&j| j < i).or_else(|| known.iter().find(|&&j| j > i))?;
                    Some(raw[*j]? + dur(i as i64 - *j as i64))
                })
            })
            .collect()
    }

    fn read_unit(&self, v: &VideoTrack, i: usize) -> crate::Result<Vec<u8>> {
        let u = v.units[i];
        if let Some((data, aus)) = &self.es {
            return Ok(data[aus[u].clone()].to_vec());
        }
        let file = self.file.as_ref().ok_or_else(|| CodecError::Container("no stream file".into()))?;
        file.read_unit(&self.bytes, v.stream, u).map_err(|e| CodecError::Container(e.to_string()))
    }

    /// Fill in the video stream info from the first picture.
    fn describe_video(&mut self, rate_from_pts: Option<f64>) -> crate::Result<()> {
        let mut seq_header = None;
        let v = self.video.as_ref().ok_or_else(|| CodecError::Unsupported("no video stream".into()))?;
        let first = self.read_unit(v, 0)?;
        let n = v.order.len();
        let (width, height, par, codec, pixel_format, mut color);
        let rate;
        color = filmcraft_color::ColorInfo::REC709;
        match v.kind {
            VKind::Mpeg2 => {
                let info = filmcraft_mpeg2v::probe(&first).ok_or_else(|| CodecError::Decode("no MPEG video sequence header at the first I picture".into()))?;
                let header = crate::video::mpeg2_sequence_header(&first).unwrap_or_default();
                // field order from the first frames' picture coding extensions
                let mut d = filmcraft_mpeg2v::Decoder::new();
                let mut fo = None;
                for i in 0..v.units.len().min(4) {
                    if let Ok(pics) = d.decode(&self.read_unit(v, i)?, i as i64)
                        && let Some(p) = pics.first()
                    {
                        fo = Some(p.field_order());
                        color = crate::video::mpeg2_to_video_frame(p.clone()).color;
                        break;
                    }
                }
                let fo = fo.unwrap_or_else(|| d.flush().first().and_then(|p| p.field_order()));
                width = info.width;
                height = info.height;
                par = info.sar;
                codec = info.codec_name();
                pixel_format = crate::video::mpeg2_pixel_format(&info, fo);
                rate = info.frame_rate.map(|(n, d)| FrameRate::new(n as i64, d as i64));
                seq_header = Some(header);
            }
            VKind::H264 | VKind::Hevc => {
                // decode up to the first frame for size, aspect and colour
                let mut dec: Box<dyn VideoDecoder> = if v.kind == VKind::H264 { Box::new(H264Decoder::annexb()) } else { Box::new(HevcDecoder::annexb()) };
                let mut frame = None;
                for i in 0..v.units.len().min(32) {
                    if let Some(f) = dec.decode(&self.read_unit(v, i)?, i as i64)?.into_iter().next() {
                        frame = Some(f.frame);
                        break;
                    }
                }
                let frame = frame.or_else(|| dec.flush().into_iter().next().map(|f| f.frame)).ok_or_else(|| CodecError::Decode("no picture decoded".into()))?;
                width = frame.width;
                height = frame.height;
                par = frame.par;
                color = frame.color;
                codec = if v.kind == VKind::H264 { "H.264".into() } else { "HEVC".into() };
                rate = None;
                pixel_format = match &frame.data {
                    filmcraft_frame::PixelData::Yuv8 { chroma, .. } => format!("YUV {} 8-bit", chroma_name(*chroma)),
                    filmcraft_frame::PixelData::Yuv16 { chroma, bits, .. } => format!("YUV {} {bits}-bit", chroma_name(*chroma)),
                    _ => String::new(),
                };
            }
        }
        // the coded rate unless the timestamps say otherwise (3:2 pulldown, field-rate coding)
        let rate = stream_rate(rate, rate_from_pts);
        let duration = rate.tick_of(n as i64);
        let total_bytes: u64 = match (&self.es, &self.file) {
            (Some((_, aus)), _) => v.units.iter().map(|&u| aus[u].len() as u64).sum(),
            (None, Some(f)) => v.units.iter().map(|&u| f.streams[v.stream].units[u].size as u64).sum(),
            _ => 0,
        };
        let secs = duration.seconds();
        if let Some(h) = seq_header
            && let Some(v) = self.video.as_mut()
        {
            v.header = h;
        }
        self.info.duration = duration;
        self.info.video = Some(VideoStreamInfo {
            width,
            height,
            frame_rate: rate,
            par,
            codec,
            pixel_format,
            color,
            has_alpha: false,
            bitrate: (secs > 0.0).then(|| (total_bytes as f64 * 8.0 / secs) as u64),
            hdr: None,
        });
        Ok(())
    }

    /// Choose and index the audio stream; `origin`: PTS of the first video frame.
    fn setup_audio(&mut self, file: File, astream: Option<usize>, origin: Option<i64>) {
        let track = astream.and_then(|ai| self.audio_track(&file, ai, origin));
        self.file = Some(file);
        self.audio = track;
    }

    /// Mark the audio stream as not decodable (it still shows in the media info).
    fn audio_unsupported(&mut self, codec_name: &str, why: String) -> Option<AudioTrack> {
        self.unsupported_audio = Some(why);
        self.info.audio_streams =
            vec![AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: format!("{codec_name} (unsupported)"), bits_per_sample: None }];
        None
    }

    fn audio_track(&mut self, file: &File, ai: usize, origin: Option<i64>) -> Option<AudioTrack> {
        let st = &file.streams[ai];
        let codec_name = st.codec.name();
        if !audio_supported(&st.codec) || st.units.is_empty() {
            let why = format!("{codec_name} audio in {} (FilmCraft has no decoder for it)", file.format.name());
            return self.audio_unsupported(&codec_name, why);
        }
        let first = match file.read_unit(&self.bytes, ai, 0) {
            Ok(b) => b,
            Err(e) => {
                return self.audio_unsupported(&codec_name, format!("{codec_name}: {e}"));
            }
        };
        let mut latm = None;
        let (codec, rate, channels, per_frame, bits) = match &st.codec {
            Codec::MpegAudio | Codec::AacAdts | Codec::Ac3 => {
                let Some(fi) = filmcraft_mpegts::frame_info(&st.codec, &first) else {
                    return self.audio_unsupported(&codec_name, format!("{codec_name}: bad frame header"));
                };
                let c = match st.codec {
                    Codec::MpegAudio => ACodec::Mpa(fi.variant),
                    Codec::AacAdts => ACodec::Adts,
                    _ => ACodec::Ac3,
                };
                (c, fi.sample_rate, fi.channels as usize, Some(fi.samples), None)
            }
            Codec::AacLatm => match crate::audio::latm_config(&first) {
                Some(cfg) => {
                    latm = Some(cfg.clone());
                    (ACodec::Latm, cfg.sample_rate, cfg.channels as usize, Some(1024), None)
                }
                None => {
                    return self.audio_unsupported(&codec_name, "AAC (LATM): unsupported StreamMuxConfig (only AAC-LC is decoded)".into());
                }
            },
            Codec::LpcmBluray => match crate::audio::bluray_lpcm_header(&first) {
                Some(h) => (ACodec::LpcmBluray, h.sample_rate, h.channels, None, Some(h.bits)),
                None => {
                    return self.audio_unsupported(&codec_name, "LPCM: bad header".into());
                }
            },
            Codec::LpcmDvd => match st.lpcm {
                Some(f) => (ACodec::LpcmDvd(f), f.sample_rate, f.channels as usize, None, Some(f.bits)),
                None => {
                    return self.audio_unsupported(&codec_name, "DVD LPCM: no format header".into());
                }
            },
            _ => return self.audio_unsupported(&codec_name, format!("{codec_name} audio")),
        };
        if codec_is_ac3_unsupported(codec) {
            return self.audio_unsupported(&codec_name, format!("{codec_name} audio (FilmCraft has no AC-3 decoder yet)"));
        }
        // DVD LPCM packets split sample groups: units own the groups that start in them
        let mut byte_starts = Vec::new();
        if let ACodec::LpcmDvd(_) = codec {
            let mut acc = 0u64;
            for u in &st.units {
                byte_starts.push(acc);
                acc += u.size as u64;
            }
            byte_starts.push(acc);
        }
        let group = |f: filmcraft_mpegts::LpcmFormat| crate::audio::dvd_lpcm_group_bytes(f.channels as usize, f.bits) as u64;
        // samples per unit
        let lens: Vec<u32> = st
            .units
            .iter()
            .enumerate()
            .map(|(k, u)| match (codec, per_frame) {
                (_, Some(n)) => n,
                (ACodec::LpcmBluray, _) => {
                    let coded = (channels + 1) & !1;
                    let bps = if bits == Some(16) { 2 } else { 3 };
                    (u.size.saturating_sub(4) as usize / (coded * bps)) as u32
                }
                (ACodec::LpcmDvd(f), _) => {
                    let g = group(f);
                    // groups whose first byte is in this unit (and that end before the stream does)
                    let total = byte_starts[st.units.len()] / g;
                    let (g0, g1) = (byte_starts[k].div_ceil(g).min(total), byte_starts[k + 1].div_ceil(g).min(total));
                    ((g1 - g0) * 2) as u32
                }
                _ => 0,
            })
            .collect();
        // place units: contiguous, re-anchored at timestamp gaps of more than half a frame (PCM
        // byte streams are always contiguous)
        let origin = origin.or_else(|| st.units.iter().find_map(|u| u.pts)).unwrap_or(0);
        let to_samples = |pts: i64| ((pts - origin) as i128 * rate as i128).div_euclid(90_000) as i64;
        let contiguous = matches!(codec, ACodec::LpcmDvd(_));
        let mut starts = Vec::with_capacity(lens.len());
        let mut next: Option<i64> = None;
        for (u, &n) in st.units.iter().zip(&lens) {
            let s = match (u.pts.map(to_samples), next) {
                (_, Some(nx)) if contiguous => nx,
                (Some(p), Some(nx)) if (p - nx).abs() * 2 <= n as i64 => nx,
                (Some(p), _) => p,
                (None, Some(nx)) => nx,
                (None, None) => 0,
            };
            starts.push(s);
            next = Some(s + n as i64);
        }
        let total = next.unwrap_or(0).max(0);
        if self.info.video.is_none() || self.unsupported_video.is_some() {
            self.info.duration = Tick::from_units(total, rate.max(1) as i64);
            if self.info.video.is_none() {
                self.info.kind = MediaKind::AudioOnly;
            }
        }
        self.info.audio_streams =
            vec![AudioStreamInfo { sample_rate: rate.max(1), channels: channels.max(1) as u32, codec: codec_name, bits_per_sample: bits }];
        Some(AudioTrack {
            stream: ai,
            codec,
            rate,
            channels: channels.max(1),
            starts,
            lens,
            latm,
            byte_starts,
            state: Mutex::new(AudioState { dec: None, cache: HashMap::new(), order: VecDeque::new(), last: None }),
        })
    }

    /// Decoded samples of audio unit `i` (cached; non-sequential access primes the decoder with
    /// the previous unit).
    fn audio_unit(&self, a: &AudioTrack, st: &mut AudioState, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
        if let Some(p) = st.cache.get(&i) {
            return Ok(p.clone());
        }
        let file = self.file.as_ref().ok_or_else(|| CodecError::Container("no stream file".into()))?;
        let read = |k: usize| file.read_unit(&self.bytes, a.stream, k).map_err(|e| CodecError::Container(e.to_string()));
        if st.dec.is_none() {
            st.dec = Some(AudioDecoder::new(a.codec, a.rate, a.channels, a.latm.clone())?);
        }
        let dec = st.dec.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?;
        if st.last.is_none_or(|l| l + 1 != i) {
            dec.reset();
            if i > 0
                && let Ok(prev) = read(i - 1)
            {
                let _ = dec.decode(&prev);
            }
        }
        let data = match a.codec {
            ACodec::LpcmDvd(f) if i + 1 < a.byte_starts.len() => {
                // the sample groups starting in this unit (the last may continue in the next)
                let g = crate::audio::dvd_lpcm_group_bytes(f.channels as usize, f.bits) as u64;
                let (b0, b1) = (a.byte_starts[i], a.byte_starts[i + 1]);
                let lo = b0.div_ceil(g) * g;
                let hi = b1.div_ceil(g) * g;
                let mut d = read(i)?;
                if hi > b1
                    && let Ok(next) = read(i + 1)
                {
                    d.extend_from_slice(&next);
                }
                let e = ((hi - b0) as usize).min(d.len());
                d[((lo - b0) as usize).min(e)..e].to_vec()
            }
            _ => read(i)?,
        };
        let out = match dec.decode(&data) {
            Ok(v) => v,
            Err(_) => vec![vec![0.0; a.lens[i] as usize]; a.channels],
        };
        st.last = Some(i);
        let p = Arc::new(out);
        st.cache.insert(i, p.clone());
        st.order.push_back(i);
        if st.order.len() > 2048
            && let Some(old) = st.order.pop_front()
        {
            st.cache.remove(&old);
        }
        Ok(p)
    }
}

fn chroma_name(c: filmcraft_frame::Chroma) -> &'static str {
    match c {
        filmcraft_frame::Chroma::C420 => "4:2:0",
        filmcraft_frame::Chroma::C422 => "4:2:2",
        filmcraft_frame::Chroma::C444 => "4:4:4",
    }
}

fn codec_is_ac3_unsupported(c: ACodec) -> bool {
    matches!(c, ACodec::Ac3) && !crate::audio::AC3_DECODER
}

/// The audio decoders behind the TS/PS audio codecs.
enum AudioDecoder {
    Packet { dec: crate::audio::PacketDecoder, codec: ACodec, latm: Option<crate::audio::LatmConfig> },
    Pcm { codec: ACodec, channels: usize },
}

impl AudioDecoder {
    fn new(codec: ACodec, rate: u32, channels: usize, latm: Option<crate::audio::LatmConfig>) -> crate::Result<Self> {
        use crate::audio::PacketDecoder;
        Ok(match codec {
            ACodec::Mpa(layer) => AudioDecoder::Packet { dec: PacketDecoder::mpeg_audio(layer, rate)?, codec, latm: None },
            ACodec::Adts | ACodec::Latm => AudioDecoder::Packet { dec: PacketDecoder::lazy_aac(), codec, latm },
            ACodec::Ac3 => AudioDecoder::Packet { dec: PacketDecoder::ac3()?, codec, latm: None },
            ACodec::LpcmBluray | ACodec::LpcmDvd(_) => AudioDecoder::Pcm { codec, channels },
        })
    }
    fn reset(&mut self) {
        if let AudioDecoder::Packet { dec, .. } = self {
            dec.reset();
        }
    }
    fn decode(&mut self, data: &[u8]) -> crate::Result<Vec<Vec<f32>>> {
        match self {
            AudioDecoder::Packet { dec, codec: ACodec::Adts, .. } => {
                let (asc, payload) = crate::audio::adts_split(data).ok_or_else(|| CodecError::Decode("bad ADTS frame".into()))?;
                dec.ensure_aac(&asc)?;
                dec.decode(payload, 0)
            }
            AudioDecoder::Packet { dec, codec: ACodec::Latm, latm } => {
                let (asc, payload) = crate::audio::latm_split(data, latm).ok_or_else(|| CodecError::Decode("bad LATM frame".into()))?;
                dec.ensure_aac(&asc)?;
                dec.decode(&payload, 0)
            }
            AudioDecoder::Packet { dec, .. } => dec.decode(data, 0),
            AudioDecoder::Pcm { codec: ACodec::LpcmBluray, .. } => {
                crate::audio::decode_bluray_lpcm(data).ok_or_else(|| CodecError::Decode("bad LPCM packet".into()))
            }
            AudioDecoder::Pcm { codec: ACodec::LpcmDvd(f), channels } => Ok(crate::audio::decode_dvd_lpcm(data, *channels, f.bits)),
            AudioDecoder::Pcm { .. } => Err(CodecError::Unsupported("PCM format".into())),
        }
    }
}

/// The video samples as a [`VideoSamples`] table.
struct MpegVideo<'a> {
    src: &'a MpegSource,
    v: &'a VideoTrack,
}

impl VideoSamples for MpegVideo<'_> {
    fn count(&self) -> usize {
        self.v.units.len()
    }
    fn pts(&self, i: usize) -> i64 {
        self.v.pts[i]
    }
    fn sync_before(&self, i: usize) -> usize {
        let i = i.min(self.v.keys.len().saturating_sub(1));
        let key = (0..=i).rev().find(|&k| self.v.keys[k]).unwrap_or(0);
        // a leading picture of an open GOP (shown before its random-access picture) needs the
        // previous GOP's pictures as references
        if key > 0 && self.v.pts[i] < self.v.pts[key] { self.sync_before(key - 1) } else { key }
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        usize::try_from(t).ok().and_then(|t| self.v.order.get(t).copied())
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        self.src.read_unit(self.v, i)
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        if let Some(why) = &self.src.unsupported_video {
            return Err(CodecError::Unsupported(why.clone()));
        }
        Ok(match self.v.kind {
            VKind::Mpeg2 => Box::new(Mpeg2Decoder::new(self.v.header.clone())),
            VKind::H264 => Box::new(H264Decoder::annexb()),
            VKind::Hevc => Box::new(HevcDecoder::annexb()),
        })
    }
}

impl MediaSource for MpegSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        if let Some(why) = &self.unsupported_video {
            return Err(MediaError::Unsupported(why.clone()));
        }
        let v = self.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        if v.order.is_empty() {
            return Err(MediaError::Decode("no pictures".into()));
        }
        let rate = self.info.frame_rate();
        let f = rate.frame_at(req.time.max(Tick::ZERO)).clamp(0, v.order.len() as i64 - 1);
        let late = filmcraft_media::cancel::catch_up().map(|m| rate.frame_at(req.time - m));
        Ok(self.gop.frame_late(&MpegVideo { src: self, v }, f, late)?)
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        if let Some(why) = &self.unsupported_audio {
            return Err(MediaError::Unsupported(why.clone()));
        }
        let a = self.audio.as_ref().ok_or(MediaError::NoStream("audio"))?;
        let ch = a.channels;
        let ratio = a.rate as f64 / sample_rate as f64;
        let exact = a.rate == sample_rate;
        let s0 = if exact { start } else { (start as f64 * ratio).floor() as i64 };
        let need = if exact { frames as i64 } else { (frames as f64 * ratio).ceil() as i64 + 2 };
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        let mut st = a.state.lock().unwrap_or_else(|e| e.into_inner());
        // units overlapping [s0, s0 + need)
        let mut i = a.starts.partition_point(|&s| s <= s0).saturating_sub(1);
        while i < a.starts.len() && a.starts[i] < s0 + need {
            if a.starts[i] + a.lens[i] as i64 > s0 {
                let pk = self.audio_unit(a, &mut st, i)?;
                for (c, dst) in src.iter_mut().enumerate() {
                    let Some(chan) = pk.get(c.min(pk.len().saturating_sub(1))) else { continue };
                    for (k, v) in chan.iter().enumerate() {
                        let pos = a.starts[i] + k as i64 - s0;
                        if pos >= 0 && (pos as usize) < dst.len() {
                            dst[pos as usize] = *v;
                        }
                    }
                }
            }
            i += 1;
        }
        drop(st);
        let mut out = AudioBuffer::silence(sample_rate, ch, frames);
        if exact {
            for (c, s) in src.into_iter().enumerate() {
                out.channels[c][..frames.min(s.len())].copy_from_slice(&s[..frames.min(s.len())]);
            }
            return Ok(out);
        }
        let frac = s0 as f64 - start as f64 * ratio;
        for k in 0..frames {
            let pos = k as f64 * ratio - frac;
            let i0 = pos.floor().max(0.0) as usize;
            let f = (pos - i0 as f64) as f32;
            for c in 0..ch {
                let x = src[c].get(i0).copied().unwrap_or(0.0);
                let y = src[c].get(i0 + 1).copied().unwrap_or(x);
                out.channels[c][k] = x + (y - x) * f;
            }
        }
        Ok(out)
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes[..bytes.len().min(65_536)]) {
        return None;
    }
    Some(MpegSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

/// [`filmcraft_media::ReaderOpener`] for MPEG transport / program / video elementary streams.
pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(head) {
        return None;
    }
    Some(MpegSource::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mutated transport stream whose PTS span gave ~0 fps panicked on open with "attempt to
    /// divide by zero" (`FrameRate::tick_of`).
    #[test]
    fn damaged_timestamps_never_give_a_zero_rate() {
        for (coded, p) in
            [(None, Some(1e-9)), (Some(FrameRate::FPS_25), Some(1e-9)), (Some(FrameRate::new(0, 0)), None), (None, Some(0.0)), (None, Some(f64::NAN))]
        {
            let r = stream_rate(coded, p);
            assert!(r.num > 0 && r.den > 0, "{coded:?} {p:?} -> {r:?}");
            let _ = r.tick_of(1000);
        }
        assert_eq!(stream_rate(Some(FrameRate::FPS_25), Some(25.0)), FrameRate::FPS_25);
        assert_eq!(stream_rate(None, Some(50.0)), FrameRate::from_f64(50.0));
    }

    #[test]
    fn presentation_drops_leading_pictures_and_orders_by_key() {
        // decode order I(2) B(0) B(1) P(5) B(3) B(4): the leading B pictures are not presented
        let keys = [2, 0, 1, 5, 3, 4];
        let (pts, order) = presentation(&keys);
        assert_eq!(order, vec![0, 4, 5, 3]);
        assert_eq!(pts, vec![0, -2, -3, 3, 1, 2]);
    }
}
