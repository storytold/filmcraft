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

const SIGNATURE: &[u8] = b"YUV4MPEG2";
/// Longest stream header or `FRAME` line read.
const MAX_LINE: usize = 64 * 1024;
/// Largest picture (luma samples) accepted.
const MAX_PIXELS: u64 = 1 << 28;

pub fn sniff(head: &[u8]) -> bool {
    head.starts_with(SIGNATURE) && matches!(head.get(SIGNATURE.len()), Some(b' ' | b'\n'))
}

/// The sample layout a `C` parameter names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    /// `None` for luma only (`mono`).
    chroma: Option<Chroma>,
    bits: u32,
    alpha: bool,
}

impl Layout {
    fn parse(c: &str) -> Option<Layout> {
        let (base, bits) = match c.split_once('p') {
            Some((base, bits)) if !bits.is_empty() && bits.bytes().all(|b| b.is_ascii_digit()) => (base, bits.parse().ok()?),
            _ => (c, 8),
        };
        let (base, bits) = match base.strip_prefix("mono") {
            Some("") => ("mono", bits),
            Some(digits) if bits == 8 => ("mono", digits.parse().ok()?),
            _ => (base, bits),
        };
        if !(8..=16).contains(&bits) {
            return None;
        }
        let (chroma, alpha) = match base {
            "420" | "420jpeg" | "420paldv" | "420mpeg2" => (Some(Chroma::C420), false),
            "422" => (Some(Chroma::C422), false),
            "444" => (Some(Chroma::C444), false),
            "444alpha" if bits == 8 => (Some(Chroma::C444), true),
            "mono" => (None, false),
            _ => return None,
        };
        Some(Layout { chroma, bits, alpha })
    }

    fn label(self) -> String {
        let sub = match (self.chroma, self.alpha) {
            (None, _) => "Monochrome",
            (Some(_), true) => "YUVA 4:4:4:4",
            (Some(c), false) => return format!("YUV {} {}-bit", c.label(), self.bits),
        };
        format!("{sub} {}-bit", self.bits)
    }
}

/// Sizes of one frame's planes in samples: luma, each chroma plane (0 for mono), alpha.
fn plane_samples(w: u32, h: u32, l: Layout) -> (usize, usize, usize) {
    let luma = w as usize * h as usize;
    let chroma = l.chroma.map_or(0, |c| {
        let (sx, sy) = c.shifts();
        w.div_ceil(1 << sx) as usize * h.div_ceil(1 << sy) as usize
    });
    (luma, chroma, if l.alpha { luma } else { 0 })
}

pub struct Y4mSource {
    info: MediaInfo,
    reader: filmcraft_media::SharedReader,
    /// File offset of each frame's planes.
    frames: Vec<u64>,
    frame_bytes: usize,
    layout: Layout,
    rate: FrameRate,
}

fn parse_ratio(s: &str) -> Option<(i64, i64)> {
    let (n, d) = s.split_once(':')?;
    Some((n.parse().ok()?, d.parse().ok()?))
}

/// The line starting at `pos` (without its newline) and the offset after it.
fn line_at(reader: &dyn filmcraft_media::reader::ByteReader, pos: u64) -> Option<(Vec<u8>, u64)> {
    let mut want = 128usize;
    loop {
        let len = (reader.len().saturating_sub(pos)).min(want as u64) as usize;
        let buf = filmcraft_media::reader::read_range(reader, pos, len).ok()?;
        if let Some(end) = buf.iter().position(|&b| b == b'\n') {
            let mut line = buf;
            line.truncate(end);
            return Some((line, pos + end as u64 + 1));
        }
        if len < want || want >= MAX_LINE {
            return None;
        }
        want = (want * 8).min(MAX_LINE);
    }
}

impl Y4mSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bad = |what: &str| CodecError::Container(format!("{name}: Y4M: {what}"));
        let (header, mut pos) = line_at(&*reader, 0).ok_or_else(|| bad("no stream header"))?;
        let header = String::from_utf8_lossy(&header).into_owned();
        let mut params = header.split(' ');
        if params.next() != Some("YUV4MPEG2") {
            return Err(bad("not a YUV4MPEG2 file"));
        }
        let (mut w, mut h, mut rate, mut par, mut colour, mut full) = (0u32, 0u32, None, (1u32, 1u32), "420jpeg".to_string(), false);
        for p in params.filter(|p| !p.is_empty()) {
            let (tag, v) = p.split_at(1);
            match tag {
                "W" => w = v.parse().map_err(|_| bad("invalid width"))?,
                "H" => h = v.parse().map_err(|_| bad("invalid height"))?,
                "F" => match parse_ratio(v) {
                    Some((n, d)) if n > 0 && d > 0 => rate = Some(FrameRate::new(n, d)),
                    _ => return Err(bad("invalid frame rate")),
                },
                "A" => {
                    if let Some((n, d)) = parse_ratio(v).filter(|&(n, d)| n > 0 && d > 0)
                        && let (Ok(n), Ok(d)) = (u32::try_from(n), u32::try_from(d))
                    {
                        par = (n, d);
                    }
                }
                "C" => colour = v.to_string(),
                "X" => full |= v.eq_ignore_ascii_case("COLORRANGE=FULL"),
                _ => {}
            }
        }
        if w == 0 || h == 0 {
            return Err(bad("missing or zero size"));
        }
        if u64::from(w) * u64::from(h) > MAX_PIXELS {
            return Err(CodecError::Unsupported(format!("{name}: Y4M frames larger than 268 million pixels")));
        }
        let layout = Layout::parse(&colour).ok_or_else(|| CodecError::Unsupported(format!("{name}: Y4M colour space {colour}")))?;
        // ffmpeg's default when the file names no rate
        let rate = rate.unwrap_or(FrameRate::FPS_25);
        let (luma, chroma, alpha) = plane_samples(w, h, layout);
        let per_sample = if layout.bits > 8 { 2 } else { 1 };
        let frame_bytes = (luma + 2 * chroma + alpha) * per_sample;

        let len = reader.len();
        let mut frames = Vec::new();
        while pos < len {
            let Some((line, data)) = line_at(&*reader, pos) else { break };
            if !line.starts_with(b"FRAME") {
                break;
            }
            let Some(end) = data.checked_add(frame_bytes as u64).filter(|&e| e <= len) else { break };
            frames.push(data);
            pos = end;
        }
        if frames.is_empty() {
            return Err(bad("no complete frames"));
        }
        let color = ColorInfo { matrix: filmcraft_frame::default_matrix(w, h), range: if full { Range::Full } else { Range::Limited }, ..ColorInfo::REC709 };
        let duration = Tick::from_rational(frames.len() as i64, rate.den, rate.num);
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
                pixel_format: layout.label(),
                color,
                has_alpha: layout.alpha,
                bitrate,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: "Y4M".into(),
            start_timecode: None,
            file_size: Some(len),
        };
        Ok(Self { info, reader, frames, frame_bytes, layout, rate })
    }

    fn frame(&self, idx: usize) -> crate::Result<VideoFrame> {
        let (Some(&at), Some(v)) = (self.frames.get(idx), self.info.video.as_ref()) else {
            return Err(CodecError::Container("Y4M: frame index out of range".into()));
        };
        let data = filmcraft_media::reader::read_range(&*self.reader, at, self.frame_bytes).map_err(|e| CodecError::Container(e.to_string()))?;
        let (luma, chroma, alpha) = plane_samples(v.width, v.height, self.layout);
        // mono: neutral chroma at 4:2:0
        let mono = Layout { chroma: Some(Chroma::C420), ..self.layout };
        let grey = plane_samples(v.width, v.height, mono).1;
        let pixels = if self.layout.bits == 8 {
            let plane = |i: usize, n: usize| Arc::new(data.get(i..i + n).map(<[u8]>::to_vec).unwrap_or_else(|| vec![0; n]));
            let (y, cb, cr) = match self.layout.chroma {
                Some(_) => (plane(0, luma), plane(luma, chroma), plane(luma + chroma, chroma)),
                None => (plane(0, luma), Arc::new(vec![128; grey]), Arc::new(vec![128; grey])),
            };
            let a = self.layout.alpha.then(|| plane(luma + 2 * chroma, alpha));
            PixelData::Yuv8 { planes: [y, cb, cr], chroma: self.layout.chroma.unwrap_or(Chroma::C420), alpha: a }
        } else {
            let max = (1u32 << self.layout.bits) - 1;
            let plane = |i: usize, n: usize| {
                Arc::new(match data.get(i * 2..(i + n) * 2) {
                    Some(b) => b.as_chunks::<2>().0.iter().map(|&s| u32::from(u16::from_le_bytes(s)).min(max) as u16).collect(),
                    None => vec![0; n],
                })
            };
            let mid = 1u16 << (self.layout.bits - 1);
            let (y, cb, cr) = match self.layout.chroma {
                Some(_) => (plane(0, luma), plane(luma, chroma), plane(luma + chroma, chroma)),
                None => (plane(0, luma), Arc::new(vec![mid; grey]), Arc::new(vec![mid; grey])),
            };
            PixelData::Yuv16 { planes: [y, cb, cr], chroma: self.layout.chroma.unwrap_or(Chroma::C420), bits: self.layout.bits, alpha: None }
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
        let last = self.frames.len().saturating_sub(1);
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
    fn colour_space_names() {
        let l = |c: Option<Chroma>, bits, alpha| Some(Layout { chroma: c, bits, alpha });
        assert_eq!(Layout::parse("420jpeg"), l(Some(Chroma::C420), 8, false));
        assert_eq!(Layout::parse("420p10"), l(Some(Chroma::C420), 10, false));
        assert_eq!(Layout::parse("422p12"), l(Some(Chroma::C422), 12, false));
        assert_eq!(Layout::parse("444alpha"), l(Some(Chroma::C444), 8, true));
        assert_eq!(Layout::parse("mono"), l(None, 8, false));
        assert_eq!(Layout::parse("mono16"), l(None, 16, false));
        for bad in ["411", "420p", "420p7", "420p17", "mono10p12", "444alphap10", "", "p10"] {
            assert_eq!(Layout::parse(bad), None, "{bad}");
        }
    }
}
