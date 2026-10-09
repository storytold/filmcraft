//! MXF media source (`filmcraft-mxf` demux, GOP-aware video via [`crate::gop`]).
//!
//! - Video: H.264 / AVC-Intra (Annex B byte stream, our decoder), MPEG-2 (D-10 / IMX, XDCAM HD /
//!   HD422; our decoder), VC-3 (DNxHD/DNxHR) and ProRes through the decoder factories. Other
//!   codings (JPEG 2000, DV, MPEG-4 visual…) are identified and reported as unsupported; the file
//!   still opens so its audio can be used.
//! - Presentation order: the index table's temporal offsets; for AVC whose index marks B pictures
//!   but carries no temporal offsets, the picture order counts of the slice headers (8.2.1).
//! - Audio: every PCM sound track (Broadcast Wave / AES3, ST 382; ST 331 AES3 elements in D-10)
//!   is presented as one multichannel stream, sample-exact.
//! - Start timecode: the material package's timecode track.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_isobmff::{FourCc, SampleEntry};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_mxf::{Codec, MxfFile, Timecode, TrackKind};
use filmcraft_time::{FrameRate, Tick};

use crate::gop::{GopCache, VideoSamples};
use crate::video::VideoDecoder;
use crate::{CodecError, make_video_decoder};

impl filmcraft_mxf::ByteSource for crate::Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

pub fn sniff(head: &[u8]) -> bool {
    filmcraft_mxf::sniff(head)
}

pub struct MxfSource {
    info: MediaInfo,
    bytes: crate::Src,
    file: MxfFile,
    vtrack: Option<usize>,
    /// Presentation position of each stored picture (overrides the container's when the order
    /// comes from the bitstream).
    vpts: Vec<i64>,
    /// Display position → stored picture.
    vorder: Vec<usize>,
    /// First presented display position, and the number presented.
    vfirst: i64,
    vcount: i64,
    video: GopCache,
    /// Why the video cannot be decoded (no decoder for the coding).
    unsupported: Option<String>,
    /// MPEG-2: the first sequence header (primes decoders that start without one).
    mpeg2_header: Vec<u8>,
    /// Sound tracks combined into one stream: (track, first stored sample frame).
    atracks: Vec<(usize, u64)>,
    /// Presented sample frames.
    aframes: u64,
}

/// Picture order counts of AVC access units (Annex B), in stored order, as presentation
/// positions: the rank of (IDR period, POC) (8.2.1.1, picture order count type 0; types 1 and 2
/// never reorder for the streams written by AVC encoders we have seen, so they keep stored order).
#[cfg(feature = "h264")]
pub fn avc_presentation_order(heads: &[Vec<u8>]) -> Option<Vec<i64>> {
    use filmcraft_bitstream::{annexb_nals, unescape_rbsp};
    use filmcraft_h264::params::{Pps, Sps};
    use filmcraft_h264::slice::{NalHeader, SliceHeader};
    let mut spss: Vec<Option<Sps>> = vec![None; 32];
    let mut ppss: Vec<Option<Pps>> = vec![None; 256];
    let (mut prev_msb, mut prev_lsb) = (0i64, 0i64);
    let mut period = 0i64;
    let mut keys: Vec<(i64, i64)> = Vec::with_capacity(heads.len());
    for (i, h) in heads.iter().enumerate() {
        let mut key = None;
        for nal in annexb_nals(h) {
            let Ok(hdr) = NalHeader::parse(nal[0]) else { continue };
            let rbsp = unescape_rbsp(&nal[1..]);
            match hdr.nal_unit_type {
                7 => {
                    let sps = Sps::parse(&rbsp).ok()?;
                    let id = sps.id as usize;
                    *spss.get_mut(id)? = Some(sps);
                }
                8 => {
                    let pps = Pps::parse(&rbsp, &spss).ok()?;
                    let id = pps.id as usize;
                    *ppss.get_mut(id)? = Some(pps);
                }
                1 | 5 => {
                    let mut sps_of = None;
                    let (sh, _, _) = SliceHeader::parse(&rbsp, hdr, |id| {
                        let pps = ppss.get(id as usize).and_then(Option::as_ref).ok_or(filmcraft_h264::Error::MissingParameterSet("PPS".into()))?;
                        let sps = spss.get(pps.sps_id as usize).and_then(Option::as_ref).ok_or(filmcraft_h264::Error::MissingParameterSet("SPS".into()))?;
                        sps_of = Some(sps);
                        Ok((pps, sps))
                    })
                    .ok()?;
                    let sps = sps_of?;
                    if sh.idr {
                        if i > 0 {
                            period += 1;
                        }
                        prev_msb = 0;
                        prev_lsb = 0;
                    }
                    let poc = match sps.pic_order_cnt_type {
                        0 => {
                            let max = 1i64 << sps.log2_max_poc_lsb;
                            let lsb = sh.pic_order_cnt_lsb as i64;
                            let msb = if lsb < prev_lsb && prev_lsb - lsb >= max / 2 {
                                prev_msb + max
                            } else if lsb > prev_lsb && lsb - prev_lsb > max / 2 {
                                prev_msb - max
                            } else {
                                prev_msb
                            };
                            let top = msb + lsb;
                            let poc = if sh.field_pic { top } else { top.min(top + sh.delta_pic_order_cnt_bottom as i64) };
                            if hdr.nal_ref_idc != 0 {
                                prev_msb = msb;
                                prev_lsb = lsb;
                            }
                            poc
                        }
                        _ => i as i64,
                    };
                    if sh.has_mmco5() {
                        // memory_management_control_operation 5: later pictures start a new period
                        period += 1;
                        prev_msb = 0;
                        prev_lsb = 0;
                        key = Some((period - 1, poc));
                    } else {
                        key = Some((period, poc));
                    }
                    break;
                }
                _ => {}
            }
        }
        keys.push(key?);
    }
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by_key(|&i| (keys[i], i));
    let mut pts = vec![0i64; keys.len()];
    for (rank, &i) in order.iter().enumerate() {
        pts[i] = rank as i64;
    }
    Some(pts)
}

/// Without the `h264` feature the slice headers can't be parsed: the stored order stands.
#[cfg(not(feature = "h264"))]
pub fn avc_presentation_order(_heads: &[Vec<u8>]) -> Option<Vec<i64>> {
    None
}

/// ProRes FourCC from the RDD 44 coding label profile byte.
fn prores_fourcc(profile: Option<u8>) -> FourCc {
    FourCc(match profile {
        Some(1) => *b"apco",
        Some(2) => *b"apcs",
        Some(4) => *b"apch",
        Some(5) => *b"ap4h",
        Some(6) => *b"ap4x",
        _ => *b"apcn",
    })
}

fn prores_name(profile: Option<u8>) -> &'static str {
    match profile {
        Some(1) => "Apple ProRes 422 Proxy",
        Some(2) => "Apple ProRes 422 LT",
        Some(3) => "Apple ProRes 422",
        Some(4) => "Apple ProRes 422 HQ",
        Some(5) => "Apple ProRes 4444",
        Some(6) => "Apple ProRes 4444 XQ",
        _ => "Apple ProRes",
    }
}

/// The start timecode as a frame count at `rate` (the media's frame rate).
fn start_frames(tc: &Timecode, rate: FrameRate) -> i64 {
    let base = tc.rounded_base.max(1) as i64;
    // the real rate of the timecode: the media rate when it rounds to the base
    let (num, den) = if (rate.as_f64().round() as i64) == base {
        (rate.num, rate.den)
    } else if tc.drop_frame {
        (base * 1000, 1001)
    } else {
        (base, 1)
    };
    // frames at the tc rate → frames at `rate` (exact when they are the same rate)
    let n = tc.start as i128 * rate.num as i128 * den as i128;
    let d = num as i128 * rate.den as i128;
    ((n + d / 2) / d) as i64
}

impl MxfSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    /// Open from a random-access reader: only the KLV headers, metadata and index are read now.
    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bytes = crate::Src(reader);
        let file = filmcraft_mxf::open(&bytes).map_err(|e| match e {
            filmcraft_mxf::Error::Unsupported(s) => CodecError::Unsupported(format!("MXF: {s}")),
            other => CodecError::Container(other.to_string()),
        })?;
        for w in &file.warnings {
            log::warn!("{name}: {w}");
        }
        let vtrack = file.track_of_kind(TrackKind::Picture);
        let atracks: Vec<usize> = file
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Sound && !t.chunks.is_empty() && t.sound_format != filmcraft_mxf::SoundFormat::Other)
            .map(|(i, _)| i)
            .collect();
        if vtrack.is_none() && atracks.is_empty() {
            return Err(CodecError::Unsupported("MXF: no playable picture or PCM sound tracks".into()));
        }
        // Video
        let mut video = None;
        let mut unsupported = None;
        let (mut vpts, mut vorder, mut vfirst, mut vcount) = (Vec::new(), Vec::new(), 0, 0);
        let mut explicit_color = None;
        let mut mpeg2_header = Vec::new();
        if let Some(vi) = vtrack {
            let t = &file.tracks[vi];
            let p = t.picture.clone().unwrap_or_default();
            vpts = t.samples.iter().map(|s| s.pts).collect();
            vorder = t.display_order.clone();
            if t.needs_reorder && matches!(t.codec, Codec::Avc { .. }) {
                let heads: Vec<Vec<u8>> = (0..t.samples.len())
                    .map(|i| {
                        let s = t.samples[i];
                        filmcraft_media::reader::read_range(&*bytes.0, s.offset, (s.size as usize).min(16 << 10)).unwrap_or_default()
                    })
                    .collect();
                match avc_presentation_order(&heads) {
                    Some(pts) => {
                        vorder = vec![0; pts.len()];
                        for (s, &d) in pts.iter().enumerate() {
                            vorder[d as usize] = s;
                        }
                        vpts = pts;
                    }
                    None => log::warn!("{name}: B pictures without temporal offsets and unreadable picture order: using stored order"),
                }
            }
            vfirst = t.first_edit_unit().min(vorder.len() as i64);
            let avail = vorder.len() as i64 - vfirst;
            vcount = t.duration.map_or(avail, |d| d.min(avail)).max(0);
            let rate = if t.edit_rate.is_valid() { FrameRate::new(t.edit_rate.num as i64, t.edit_rate.den as i64) } else { FrameRate::FPS_25 };
            // MPEG-2 (D-10 / IMX, XDCAM): the sequence header of the first picture
            let mpeg = (t.codec == Codec::Mpeg2)
                .then(|| file.read_sample(&bytes, vi, 0).ok())
                .flatten()
                .and_then(|s| Some((filmcraft_mpeg2v::probe(&s)?, filmcraft_mpeg2v::scan_access_unit(&s), crate::video::mpeg2_sequence_header(&s)?)));
            let (w, h) = match &mpeg {
                Some((info, _, _)) => (info.width, info.height),
                None => (p.frame_width(), p.frame_height()),
            };
            let par = if p.aspect_ratio.is_valid() && w > 0 && h > 0 {
                let n = p.aspect_ratio.num as u64 * h as u64;
                let d = p.aspect_ratio.den as u64 * w as u64;
                let g = gcd(n, d).max(1);
                ((n / g) as u32, (d / g) as u32)
            } else {
                (1, 1)
            };
            let codec = match t.codec {
                Codec::Vc3 => file
                    .read_sample(&bytes, vi, 0)
                    .ok()
                    .and_then(|f| filmcraft_dnx::probe(&f).ok())
                    .map(|h| filmcraft_dnx::cid_name(h.cid))
                    .unwrap_or_else(|| "DNxHD/DNxHR".into()),
                Codec::ProRes { profile } => prores_name(profile).into(),
                c @ Codec::Avc { .. } => c.name().into(),
                Codec::Mpeg2 if mpeg.is_some() => mpeg.as_ref().map(|m| m.0.codec_name()).unwrap_or_default(),
                other => {
                    let why = format!("{} video in MXF (FilmCraft has no {} decoder)", other.name(), other.name());
                    unsupported = Some(why);
                    format!("{} (unsupported)", other.name())
                }
            };
            let chroma = match (p.horizontal_subsampling, p.vertical_subsampling) {
                (2, 2) => "4:2:0",
                (2, 1) => "4:2:2",
                (1, 1) => "4:4:4",
                _ => "",
            };
            let mut pixel_format = if p.component_depth > 0 && !chroma.is_empty() { format!("YUV {chroma} {}-bit", p.component_depth) } else { String::new() };
            if let Some((info, au, header)) = &mpeg {
                let fo = au.pictures.first().and_then(|(ph, pce)| {
                    let pce = pce.as_ref()?;
                    let _ = ph;
                    (!pce.progressive_frame).then_some(if pce.picture_structure == 2 || (pce.picture_structure == 3 && !pce.top_field_first) {
                        filmcraft_mpeg2v::FieldOrder::BottomFirst
                    } else {
                        filmcraft_mpeg2v::FieldOrder::TopFirst
                    })
                });
                pixel_format = crate::video::mpeg2_pixel_format(info, fo);
                mpeg2_header = header.clone();
            }
            let mut color = filmcraft_color::ColorInfo { matrix: filmcraft_frame::default_matrix(w, h), ..filmcraft_color::ColorInfo::REC709 };
            if p.full_range() {
                color.range = filmcraft_color::Range::Full;
                explicit_color = Some(color);
            }
            let secs = vcount as f64 / rate.as_f64().max(1e-9);
            let bytes_total: u64 = t.samples.iter().map(|s| s.size as u64).sum();
            video = Some(VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: rate,
                par,
                codec,
                pixel_format,
                color,
                has_alpha: p.alpha_depth > 0,
                bitrate: (secs > 0.0).then(|| (bytes_total as f64 * 8.0 / (t.samples.len().max(1) as f64 / rate.as_f64().max(1e-9))) as u64),
                hdr: None,
            });
        }
        // Audio: all PCM sound tracks of the first track's rate, channels in track order
        let mut audio = None;
        let mut combined = Vec::new();
        let mut aframes = u64::MAX;
        if let Some(&a0) = atracks.first() {
            let si0 = file.tracks[a0].sound.clone().unwrap_or_default();
            let rate = si0.sample_rate;
            let mut channels = 0;
            for &ai in &atracks {
                let t = &file.tracks[ai];
                let Some(si) = t.sound.as_ref() else { continue };
                if si.sample_rate != rate {
                    continue;
                }
                let (n, d) = t.samples_per_edit_unit();
                let first = (t.first_edit_unit() as i128 * n as i128 / d.max(1) as i128) as u64;
                let stored = t.stored_sample_frames().saturating_sub(first);
                let presented = t.duration.map_or(stored, |du| ((du as i128 * n as i128 / d.max(1) as i128) as u64).min(stored));
                aframes = aframes.min(presented);
                channels += si.channels.max(1);
                combined.push((ai, first));
            }
            let sr = if rate.is_valid() { (rate.num as f64 / rate.den as f64).round() as u32 } else { 48_000 };
            audio = Some(AudioStreamInfo { sample_rate: sr.max(1), channels, codec: file.tracks[a0].codec.name().into(), bits_per_sample: Some(si0.bits) });
        }
        if aframes == u64::MAX {
            aframes = 0;
        }
        let duration = match (&video, &audio) {
            (Some(v), _) => v.frame_rate.tick_of(vcount),
            (None, Some(a)) => Tick::from_units(aframes as i64, a.sample_rate as i64),
            _ => Tick::ZERO,
        };
        let mut info = MediaInfo {
            name: name.to_string(),
            kind: if video.is_some() { MediaKind::Movie } else { MediaKind::AudioOnly },
            duration,
            video,
            audio,
            container: format!("MXF {}", file.operational_pattern.name()),
            start_timecode: None,
            file_size: Some(bytes.0.len()),
        };
        info.start_timecode = file.timecode.as_ref().map(|tc| start_frames(tc, info.frame_rate()));
        Ok(Self {
            info,
            bytes,
            vtrack,
            vpts,
            vorder,
            vfirst,
            vcount,
            video: GopCache::new(explicit_color),
            unsupported,
            mpeg2_header,
            atracks: combined,
            aframes,
            file,
        })
    }

    /// The demuxed file (tracks, partitions, index, timecode).
    pub fn file(&self) -> &MxfFile {
        &self.file
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The MXF picture track as a [`VideoSamples`] table (pts in display positions).
struct MxfVideo<'a> {
    src: &'a MxfSource,
    track: usize,
}

impl VideoSamples for MxfVideo<'_> {
    fn count(&self) -> usize {
        self.src.vpts.len()
    }
    fn pts(&self, i: usize) -> i64 {
        self.src.vpts[i]
    }
    fn sync_before(&self, i: usize) -> usize {
        let key = self.src.file.tracks[self.track].sync_before(i);
        // a leading picture of an open GOP (stored after its random-access picture, shown
        // before it) is predicted from the previous GOP too
        if key > 0 && i < self.src.vpts.len() && self.src.vpts[i] < self.src.vpts[key] { self.sync_before(key - 1) } else { key }
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        usize::try_from(t).ok().and_then(|t| self.src.vorder.get(t).copied())
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        self.src.file.read_sample(&self.src.bytes, self.track, i).map_err(|e| CodecError::Container(e.to_string()))
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        if let Some(why) = &self.src.unsupported {
            return Err(CodecError::Unsupported(why.clone()));
        }
        let t = &self.src.file.tracks[self.track];
        let v = self.src.info.video.as_ref().ok_or_else(|| CodecError::Unsupported("no video track".into()))?;
        let (w, h) = (v.width.min(u16::MAX as u32) as u16, v.height.min(u16::MAX as u32) as u16);
        match t.codec {
            Codec::Avc { .. } => crate::video::h264_annexb(),
            Codec::Mpeg2 => Ok(Box::new(crate::video::Mpeg2Decoder::new(self.src.mpeg2_header.clone()))),
            Codec::Vc3 => make_video_decoder(&SampleEntry::dnx(FourCc(*b"AVdh"), w, h)),
            Codec::ProRes { profile } => make_video_decoder(&SampleEntry::prores(prores_fourcc(profile), w, h)),
            other => Err(CodecError::Unsupported(format!("{} video in MXF", other.name()))),
        }
    }
}

impl MediaSource for MxfSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let ti = self.vtrack.ok_or(MediaError::NoStream("video"))?;
        if let Some(why) = &self.unsupported {
            return Err(MediaError::Unsupported(why.clone()));
        }
        if self.vcount == 0 {
            return Err(MediaError::Decode("MXF: no pictures".into()));
        }
        let rate = self.info.frame_rate();
        let f = rate.frame_at(req.time.max(Tick::ZERO)).clamp(0, self.vcount - 1);
        let late = filmcraft_media::cancel::catch_up().map(|m| self.vfirst + rate.frame_at(req.time - m));
        let frame = self.video.frame_late(&MxfVideo { src: self, track: ti }, self.vfirst + f, late)?;
        Ok(frame)
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let ainfo = self.info.audio.as_ref().ok_or(MediaError::NoStream("audio"))?;
        let src_rate = ainfo.sample_rate;
        let ch = ainfo.channels.max(1) as usize;
        let mut out = AudioBuffer::silence(sample_rate, ch, frames);
        let ratio = src_rate as f64 / sample_rate as f64;
        let exact = src_rate == sample_rate;
        let s0 = if exact { start } else { (start as f64 * ratio).floor() as i64 };
        let need = if exact { frames as i64 } else { (frames as f64 * ratio).ceil() as i64 + 2 };
        // clip the request to the presented range [0, aframes)
        let a = s0.max(0);
        let b = (s0 + need).min(self.aframes as i64);
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        if b > a {
            let mut c0 = 0;
            for &(ti, first) in &self.atracks {
                let pcm = self.file.read_pcm(&self.bytes, ti, first + a as u64, (b - a) as usize).map_err(|e| MediaError::Decode(e.to_string()))?;
                for (k, p) in pcm.into_iter().enumerate() {
                    if let Some(dst) = src.get_mut(c0 + k) {
                        let at = (a - s0) as usize;
                        dst[at..at + p.len()].copy_from_slice(&p);
                    }
                }
                c0 += self.file.tracks[ti].sound.as_ref().map_or(1, |s| s.channels.max(1) as usize);
            }
        }
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

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes[..bytes.len().min(70_000)]) {
        return None;
    }
    Some(MxfSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

/// [`filmcraft_media::ReaderOpener`] for MXF.
pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(head) {
        return None;
    }
    Some(MxfSource::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_frames_at_the_media_rate() {
        let tc = Timecode { start: 90_000, rounded_base: 25, drop_frame: false };
        assert_eq!(start_frames(&tc, FrameRate::FPS_25), 90_000);
        // 50p media with a 25 fps timecode track
        assert_eq!(start_frames(&tc, FrameRate::FPS_50), 180_000);
        // 29.97 DF: the frame count is already at 29.97
        let df = Timecode { start: 107_892, rounded_base: 30, drop_frame: true };
        assert_eq!(start_frames(&df, FrameRate::FPS_29_97), 107_892);
        // audio-only media (default 23.976): one hour of DF timecode is one real hour minus 3.6 s
        let f = start_frames(&df, FrameRate::FPS_23_976);
        let secs = f as f64 * 1001.0 / 24000.0;
        assert!((secs - 107_892.0 * 1001.0 / 30000.0).abs() < 0.05, "{secs}");
    }
}
