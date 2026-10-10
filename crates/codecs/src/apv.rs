//! Standalone APV raw bitstream source (RFC 9924 Appendix A: `.apv` elementary streams).
//!
//! Each access unit in a raw bitstream is prefixed by a 4-byte big-endian `au_size` followed by
//! the `'aPv1'` signature (unprefixed single-AU streams starting directly with `'aPv1'` are also
//! accepted). Every frame is intra-coded, so random access is direct.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, Matrix, Range, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::CodecError;
use crate::gop::{GopCache, VideoSamples};
use crate::video::{ApvDecoder, VideoDecoder, primaries_from_code};

/// Sniff a raw APV bitstream: either a `raw_bitstream_access_unit()` (`[au_size: u32]['aPv1']`) or
/// an unprefixed `access_unit()` starting with `'aPv1'`.
pub fn sniff(head: &[u8]) -> bool {
    (head.len() >= 8 && &head[4..8] == b"aPv1" && u32::from_be_bytes([head[0], head[1], head[2], head[3]]) >= 8) || head.starts_with(b"aPv1")
}

pub struct ApvSource {
    info: MediaInfo,
    bytes: crate::Src,
    samples: Vec<(u64, u32)>,
    rate: FrameRate,
    video: GopCache,
}

pub(crate) fn apv_pixfmt_label(chroma: filmcraft_apv::ChromaFormat, bit_depth: u8) -> String {
    let sub = match chroma {
        filmcraft_apv::ChromaFormat::Monochrome => "Monochrome",
        filmcraft_apv::ChromaFormat::Yuv422 => "YUV 4:2:2",
        filmcraft_apv::ChromaFormat::Yuv444 => "YUV 4:4:4",
        filmcraft_apv::ChromaFormat::Yuv4444 => "YUVA 4:4:4:4",
    };
    format!("{sub} {bit_depth}-bit")
}

impl ApvSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let len = reader.len();
        let mut samples = Vec::new();
        let mut pos = 0u64;
        let mut hdr = [0u8; 8];
        while pos + 8 <= len {
            if reader.read_at(pos, &mut hdr).is_err() {
                break;
            }
            if &hdr[4..8] == b"aPv1" {
                let sz = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                if sz < 8 || pos + 4 + sz as u64 > len {
                    break;
                }
                samples.push((pos + 4, sz));
                pos += 4 + sz as u64;
            } else if pos == 0 && &hdr[0..4] == b"aPv1" && len <= u32::MAX as u64 {
                samples.push((0, len as u32));
                break;
            } else {
                break;
            }
        }
        let Some(&(off0, sz0)) = samples.first() else {
            return Err(CodecError::Container("APV: no valid access units".into()));
        };
        let first = filmcraft_media::reader::read_range(&*reader, off0, sz0 as usize).map_err(|e| CodecError::Container(e.to_string()))?;
        let fh = filmcraft_apv::probe(&first).map_err(|e| CodecError::Decode(e.to_string()))?;
        let mut color = ColorInfo { matrix: filmcraft_frame::default_matrix(fh.width, fh.height), ..ColorInfo::REC709 };
        let mut explicit_color = None;
        if fh.color_description_present {
            if let Some(m) = Matrix::from_code(fh.color.matrix) {
                color.matrix = m;
            }
            if let Some(t) = Transfer::from_code(fh.color.transfer) {
                color.transfer = t;
            }
            if let Some(p) = primaries_from_code(fh.color.primaries) {
                color.primaries = p;
            }
            if fh.color.full_range {
                color.range = Range::Full;
            }
            explicit_color = Some(color);
        }
        let rate = FrameRate::FPS_24;
        let duration = Tick::from_rational(samples.len() as i64 * rate.den, 1, rate.num);
        let total_bits: u64 = samples.iter().map(|s| s.1 as u64 * 8).sum();
        let secs = duration.seconds();
        let bitrate = (secs > 0.0).then(|| (total_bits as f64 / secs) as u64);
        let codec = match filmcraft_apv::Profile::from_idc(fh.profile_idc) {
            Some(p) => p.name().into(),
            None => "APV".into(),
        };
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Movie,
            duration,
            video: Some(VideoStreamInfo {
                width: fh.width,
                height: fh.height,
                frame_rate: rate,
                par: (1, 1),
                codec,
                pixel_format: apv_pixfmt_label(fh.chroma, fh.bit_depth),
                color,
                has_alpha: fh.chroma == filmcraft_apv::ChromaFormat::Yuv4444,
                bitrate,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: "APV".into(),
            start_timecode: None,
            file_size: Some(len),
        };
        Ok(Self { info, bytes: crate::Src(reader), samples, rate, video: GopCache::new(explicit_color) })
    }
}

struct ApvVideo<'a>(&'a ApvSource);

impl VideoSamples for ApvVideo<'_> {
    fn count(&self) -> usize {
        self.0.samples.len()
    }
    fn pts(&self, i: usize) -> i64 {
        i as i64
    }
    fn sync_before(&self, i: usize) -> usize {
        i
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        (!self.0.samples.is_empty()).then(|| t.clamp(0, self.0.samples.len() as i64 - 1) as usize)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        let (off, sz) = self.0.samples.get(i).copied().ok_or_else(|| CodecError::Container("sample index out of range".into()))?;
        filmcraft_media::reader::read_range(&*self.0.bytes.0, off, sz as usize).map_err(|e| CodecError::Container(e.to_string()))
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        Ok(Box::new(ApvDecoder))
    }
}

impl MediaSource for ApvSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let frame_idx = self.rate.frame_at(req.time.max(Tick::ZERO));
        Ok(self.video.frame(&ApvVideo(self), frame_idx)?)
    }

    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        Err(MediaError::NoStream("audio"))
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes) {
        return None;
    }
    Some(ApvSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(head) {
        return None;
    }
    Some(ApvSource::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}
