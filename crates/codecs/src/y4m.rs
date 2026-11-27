//! YUV4MPEG2 (`.y4m`) files: uncompressed planar video, as described by the format's public
//! documentation (the `yuv4mpeg(5)` manual page of mjpegtools, and the `C` colour space names
//! other tools added for high bit depths).
//!
//! The file starts with `YUV4MPEG2` and space-separated tagged parameters up to a newline: `W` and
//! `H` (size), `F` (frame rate, `n:d`), `A` (pixel aspect, `n:d`, `0:0` unknown), `I` (interlacing),
//! `C` (colour space; `420jpeg` when absent) and `X` (extensions; `XCOLORRANGE=FULL` is read). Each
//! frame is a `FRAME` line (with optional parameters of its own) followed by the Y, Cb and Cr planes
//! (and an alpha plane for `444alpha`); samples above 8 bits are 16-bit little-endian.
//!
//! Frames are found once at open (their header lines only) and read on demand, so a large file
//! is never loaded whole. Every number comes from the file and is checked before use.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, Range};
use filmcraft_frame::{AudioBuffer, Chroma, PixelData, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::CodecError;

pub use craft_y4m::sniff;

fn chroma(c: craft_y4m::Chroma) -> Chroma {
    match c {
        craft_y4m::Chroma::C420 => Chroma::C420,
        craft_y4m::Chroma::C422 => Chroma::C422,
        craft_y4m::Chroma::C444 => Chroma::C444,
    }
}
fn layout_label(layout: craft_y4m::Layout) -> String {
    let sub = match (layout.chroma, layout.alpha) {
        (None, _) => "Monochrome",
        (Some(_), true) => "YUVA 4:4:4:4",
        (Some(c), false) => return format!("YUV {} {}-bit", chroma(c).label(), layout.bits),
    };
    format!("{sub} {}-bit", layout.bits)
}

pub struct Y4mSource {
    info: MediaInfo,
    reader: filmcraft_media::SharedReader,
    stream: craft_y4m::Stream,
    rate: FrameRate,
}

impl Y4mSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let len = reader.len();
        let stream = craft_y4m::Stream::open(len, |at, n| filmcraft_media::reader::read_range(&*reader, at, n).ok(), &craft_y4m::Limits::default()).map_err(
            |e| match e {
                craft_y4m::Error::UnsupportedColourSpace(c) => CodecError::Unsupported(format!("{name}: Y4M colour space {c}")),
                craft_y4m::Error::LimitExceeded(_) => CodecError::Unsupported(format!("{name}: Y4M frames larger than 268 million pixels")),
                craft_y4m::Error::Malformed(m) => CodecError::Container(format!("{name}: Y4M: {m}")),
            },
        )?;
        let hinfo = &stream.header;
        let (w, h) = (hinfo.width, hinfo.height);
        let rate = hinfo.frame_rate.map(|(n, d)| FrameRate::new(n, d)).unwrap_or(FrameRate::FPS_25);
        let par = hinfo.pixel_aspect.unwrap_or((1, 1));
        let layout = hinfo.layout;
        let full = hinfo.full_range;
        let frame_bytes = stream.frame_bytes;
        let color = ColorInfo { matrix: filmcraft_frame::default_matrix(w, h), range: if full { Range::Full } else { Range::Limited }, ..ColorInfo::REC709 };
        let duration = Tick::from_rational(stream.frames.len() as i64, rate.den, rate.num);
        let bitrate = (frame_bytes as u64 * 8).checked_mul(rate.num as u64).map(|b| b / rate.den as u64);
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Movie,
            duration,
            video: Some(VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: rate,
                par,
                codec: "Uncompressed YUV".into(),
                pixel_format: layout_label(layout),
                color,
                has_alpha: layout.alpha,
                bitrate,
                hdr: None,
                color_assumed: Some(filmcraft_color::ColorAssumed::ALL),
            }),
            audio_streams: Vec::new(),
            container: "Y4M".into(),
            start_timecode: None,
            file_size: Some(len),
        };
        Ok(Self { info, reader, stream, rate })
    }

    fn frame(&self, idx: usize) -> crate::Result<VideoFrame> {
        let (Some(&at), Some(v)) = (self.stream.frames.get(idx), self.info.video.as_ref()) else {
            return Err(CodecError::Container("Y4M: frame index out of range".into()));
        };
        let data = filmcraft_media::reader::read_range(&*self.reader, at, self.stream.frame_bytes).map_err(|e| CodecError::Container(e.to_string()))?;
        let header = &self.stream.header;
        let layout = header.layout;
        let (luma, chroma_samples, alpha) = header.plane_samples();
        let grey = header.chroma_samples(craft_y4m::Chroma::C420);
        let pixels = if layout.bits == 8 {
            let plane = |i: usize, n: usize| Arc::new(data.get(i..i + n).map(<[u8]>::to_vec).unwrap_or_else(|| vec![0; n]));
            let (y, cb, cr) = match layout.chroma {
                Some(_) => (plane(0, luma), plane(luma, chroma_samples), plane(luma + chroma_samples, chroma_samples)),
                None => (plane(0, luma), Arc::new(vec![128; grey]), Arc::new(vec![128; grey])),
            };
            let a = layout.alpha.then(|| plane(luma + 2 * chroma_samples, alpha));
            PixelData::Yuv8 { planes: [y, cb, cr], chroma: layout.chroma.map(chroma).unwrap_or(Chroma::C420), alpha: a }
        } else {
            let plane = |i: usize, n: usize| {
                Arc::new(match data.get(i * 2..(i + n) * 2) {
                    Some(b) => craft_y4m::samples_u16(b, layout.bits),
                    None => vec![0; n],
                })
            };
            let mid = 1u16 << (layout.bits - 1);
            let (y, cb, cr) = match layout.chroma {
                Some(_) => (plane(0, luma), plane(luma, chroma_samples), plane(luma + chroma_samples, chroma_samples)),
                None => (plane(0, luma), Arc::new(vec![mid; grey]), Arc::new(vec![mid; grey])),
            };
            PixelData::Yuv16 { planes: [y, cb, cr], chroma: layout.chroma.map(chroma).unwrap_or(Chroma::C420), bits: layout.bits, alpha: None }
        };
        Ok(VideoFrame {
            width: v.width,
            height: v.height,
            data: pixels,
            color: v.color,
            par: v.par,
            pts: Tick::from_rational(idx as i64, self.rate.den, self.rate.num),
        })
    }
}

impl MediaSource for Y4mSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let last = self.stream.frames.len().saturating_sub(1);
        let idx = usize::try_from(self.rate.frame_at(req.time.max(Tick::ZERO))).unwrap_or(last).min(last);
        Ok(Arc::new(self.frame(idx)?))
    }

    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        Err(MediaError::NoStream("audio"))
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes) {
        return None;
    }
    Some(Y4mSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(head) {
        return None;
    }
    Some(Y4mSource::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_unknown_header_tag_is_safe() {
        let mut bytes = b"YUV4MPEG2 W2 H2 F25:1 Cmono ".to_vec();
        bytes.extend_from_slice("é".as_bytes());
        bytes.extend_from_slice(b"\nFRAME\n");
        bytes.extend_from_slice(&[10, 20, 30, 40]);
        let source = Y4mSource::open("unicode.y4m", bytes.into()).unwrap();
        assert_eq!(source.info().video.as_ref().unwrap().width, 2);
        assert!(source.frame(0).is_ok());
    }

    #[test]
    fn colour_space_names() {
        let l = |c: Option<craft_y4m::Chroma>, bits, alpha| Some(craft_y4m::Layout { chroma: c, bits, alpha });
        assert_eq!(craft_y4m::Layout::parse("420jpeg"), l(Some(craft_y4m::Chroma::C420), 8, false));
        assert_eq!(craft_y4m::Layout::parse("420p10"), l(Some(craft_y4m::Chroma::C420), 10, false));
        assert_eq!(craft_y4m::Layout::parse("422p12"), l(Some(craft_y4m::Chroma::C422), 12, false));
        assert_eq!(craft_y4m::Layout::parse("444alpha"), l(Some(craft_y4m::Chroma::C444), 8, true));
        assert_eq!(craft_y4m::Layout::parse("mono"), l(None, 8, false));
        assert_eq!(craft_y4m::Layout::parse("mono16"), l(None, 16, false));
        for bad in ["411", "420p", "420p7", "420p17", "mono10p12", "444alphap10", "", "p10"] {
            assert_eq!(craft_y4m::Layout::parse(bad), None, "{bad}");
        }
    }
}
