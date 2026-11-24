//! Standalone APV raw bitstream source (RFC 9924 Appendix A: `.apv` elementary streams).
//!
//! Each access unit in a raw bitstream is prefixed by a 4-byte big-endian `au_size` followed by
//! the `'aPv1'` signature (unprefixed single-AU streams starting directly with `'aPv1'` are also
//! accepted). Every frame is intra-coded, so random access is direct.

use std::sync::Arc;

use craft_video::apv::{RawAccessUnit, raw_access_units};
use filmcraft_color::{ColorInfo, Matrix, Range, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::CodecError;
use crate::gop::{GopCache, VideoSamples};
use crate::video::{ApvDecoder, VideoDecoder, primaries_from_code};

pub use craft_video::apv::sniff_raw_bitstream as sniff;

pub struct ApvSource {
    info: MediaInfo,
    bytes: crate::Src,
    samples: Vec<RawAccessUnit>,
    rate: FrameRate,
    video: GopCache,
}

pub(crate) fn apv_pixfmt_label(chroma: craft_video::apv::ChromaFormat, bit_depth: u8) -> String {
    let sub = match chroma {
        craft_video::apv::ChromaFormat::Monochrome => "Monochrome",
        craft_video::apv::ChromaFormat::Yuv422 => "YUV 4:2:2",
        craft_video::apv::ChromaFormat::Yuv444 => "YUV 4:4:4",
        craft_video::apv::ChromaFormat::Yuv4444 => "YUVA 4:4:4:4",
    };
    format!("{sub} {bit_depth}-bit")
}

impl ApvSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let len = reader.len();
        let bytes = crate::Src(reader);
        let mut samples = Vec::new();
        for access_unit in raw_access_units(&bytes) {
            samples.try_reserve(1).map_err(|_| CodecError::Container("APV: could not allocate access-unit index".into()))?;
            samples.push(access_unit);
        }
        let Some(first) = samples.first() else {
            return Err(CodecError::Container("APV: no valid access units".into()));
        };
        let first = filmcraft_media::reader::read_range(&*bytes.0, first.offset, first.size as usize).map_err(|e| CodecError::Container(e.to_string()))?;
        let fh = craft_video::apv::probe(&first).map_err(|e| CodecError::Decode(e.to_string()))?;
        let mut color = ColorInfo { matrix: filmcraft_frame::default_matrix(fh.width, fh.height), ..ColorInfo::REC709 };
        let mut explicit_color = None;
        let codes = fh.color_description_present.then_some(crate::stream_color::ColorCodes {
            primaries: fh.color.primaries,
            transfer: fh.color.transfer,
            matrix: fh.color.matrix,
            full_range: Some(fh.color.full_range),
        });
        let assumed = crate::stream_color::assumed(codes.as_slice());
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
        let total_bits = samples.iter().map(|sample| u64::from(sample.size)).sum::<u64>().saturating_mul(8);
        let secs = duration.seconds();
        let bitrate = (secs > 0.0).then(|| (total_bits as f64 / secs) as u64);
        let codec = match craft_video::apv::Profile::from_idc(fh.profile_idc) {
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
                has_alpha: fh.chroma == craft_video::apv::ChromaFormat::Yuv4444,
                bitrate,
                hdr: None,
                color_assumed: Some(assumed),
            }),
            audio_streams: Vec::new(),
            container: "APV".into(),
            start_timecode: None,
            file_size: Some(len),
        };
        Ok(Self { info, bytes, samples, rate, video: GopCache::new(explicit_color) })
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
        let sample = self.0.samples.get(i).ok_or_else(|| CodecError::Container("sample index out of range".into()))?;
        filmcraft_media::reader::read_range(&*self.0.bytes.0, sample.offset, sample.size as usize).map_err(|e| CodecError::Container(e.to_string()))
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

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn standalone_apv_keeps_complete_pictures_before_a_short_tail() {
        use craft_video::apv::{ChromaFormat, Encoder, Frame, Profile};
        use filmcraft_media::reader::{FileReader, open_reader};

        // Original test frames with distinct luma, produced by the shared APV encoder.
        let (w, h) = (32, 16);
        let mut encoder = Encoder::new(Profile::P422_10, w, h).unwrap();
        let mut frame = Frame::new(w, h, ChromaFormat::Yuv422, 10, false);
        frame.y.fill(160);
        let first = encoder.encode_raw_au(&frame).unwrap();
        frame.y.fill(760);
        let second = encoder.encode_raw_au(&frame).unwrap();

        let bare = crate::open_bytes("bare.apv", first[4..].into()).unwrap();
        assert_eq!(bare.info().duration, FrameRate::FPS_24.tick_of(1));
        let reference = bare.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().to_rgba8();

        let data: Arc<[u8]> = [&first[..], &second[..], &first[..first.len() - 4]].concat().into();
        let memory = crate::open_bytes("prefix.apv", data.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prefix.apv");
        std::fs::write(&path, &data).unwrap();
        let file = Arc::new(FileReader::open(&path).unwrap());
        let disk = open_reader("prefix.apv", file, &crate::reader_openers(), &crate::openers()).unwrap();

        for source in [disk, memory] {
            let info = source.info();
            let video = info.video.as_ref().unwrap();
            assert_eq!((video.width, video.height, video.frame_rate), (w, h, FrameRate::FPS_24));
            assert_eq!(info.duration, FrameRate::FPS_24.tick_of(2));
            assert_eq!(info.container, "APV");
            assert!(!info.has_audio());
            let at = |slot| source.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(slot))).unwrap().to_rgba8();
            let later = at(1);
            assert_ne!(later, reference);
            assert_eq!(at(0), reference);
            assert_eq!(at(2), later, "seeking past the complete prefix holds its last picture");
        }
    }
}
