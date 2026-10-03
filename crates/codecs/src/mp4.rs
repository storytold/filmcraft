//! MP4/MOV media source with GOP-aware random access (see [`crate::gop`]).
//!
//! Opus (`Opus` sample entry + `dOps`): always 48 kHz output. Pre-skip is removed by the edit list
//! (`media_time` = pre-skip), or applied from `dOps` when a file has no edit list; random access
//! decodes [`crate::audio::OPUS_PRE_ROLL`] of preceding packets first (≥ the 80 ms `roll` distance).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_isobmff::{CodecConfig, Mp4File, TrackKind};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::audio::{PacketDecoder, decode_pcm};
use crate::gop::{GopCache, VideoSamples};
use crate::video::VideoDecoder;
use crate::{CodecError, make_video_decoder};

struct AudioState {
    decoder: Option<PacketDecoder>,
    /// Decoded packets by sample index.
    packets: HashMap<usize, Arc<Vec<Vec<f32>>>>,
    order: Vec<usize>,
    last_decoded: Option<usize>,
}

pub struct Mp4Source {
    info: MediaInfo,
    bytes: crate::Src,
    file: Mp4File,
    vtrack: Option<usize>,
    atrack: Option<usize>,
    video: GopCache,
    audio: Mutex<AudioState>,
    /// Cumulative sample start frames for the audio track (for packet lookup).
    audio_starts: Vec<i64>,
    /// Presentation offset of the audio track in its timescale (edit list, or Opus pre-skip).
    audio_offset: i64,
    /// Decoder pre-roll after a seek, in audio track timescale units (0: prime with one packet).
    audio_preroll: i64,
}

pub fn sniff(b: &[u8]) -> bool {
    b.len() >= 12 && matches!(&b[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip")
}

fn color_from(entry: &filmcraft_isobmff::SampleEntry, w: u32, h: u32) -> ColorInfo {
    let mut c = ColorInfo { matrix: filmcraft_frame::default_matrix(w, h), transfer: Transfer::Bt709, primaries: Primaries::Bt709, range: Range::Limited };
    // VP9 carries its colour description in vpcC (used when there is no colr box).
    let vpc = match &entry.codec {
        CodecConfig::Vp9(v) => Some(filmcraft_isobmff::ColorInfo::Nclx {
            primaries: v.colour_primaries as u16,
            transfer: v.transfer_characteristics as u16,
            matrix: v.matrix_coefficients as u16,
            full_range: v.full_range,
        }),
        _ => None,
    };
    if let Some(col) = entry.video.as_ref().and_then(|v| v.color.as_ref()).or(vpc.as_ref()) {
        let (p, t, m, full) = match col {
            filmcraft_isobmff::ColorInfo::Nclx { primaries, transfer, matrix, full_range } => (*primaries, *transfer, *matrix, *full_range),
            filmcraft_isobmff::ColorInfo::Nclc { primaries, transfer, matrix } => (*primaries, *transfer, *matrix, false),
            _ => return c,
        };
        if let Some(m) = Matrix::from_code(m as u8) {
            c.matrix = m;
        }
        if let Some(t) = Transfer::from_code(t as u8) {
            c.transfer = t;
        }
        c.primaries = match p {
            9 => Primaries::Bt2020,
            12 => Primaries::P3D65,
            5 => Primaries::Bt601_625,
            6 => Primaries::Bt601_525,
            _ => Primaries::Bt709,
        };
        if full {
            c.range = Range::Full;
        }
    }
    c
}

impl Mp4Source {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    /// Open from a random-access reader: only the index is read now, samples on demand.
    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bytes = crate::Src(reader);
        let file = filmcraft_isobmff::open(&bytes).map_err(|e| CodecError::Container(e.to_string()))?;
        let vtrack = file.tracks.iter().position(|t| t.kind == TrackKind::Video && !t.samples.is_empty());
        let atrack = file.tracks.iter().position(|t| t.kind == TrackKind::Audio && !t.samples.is_empty());
        if vtrack.is_none() && atrack.is_none() {
            return Err(CodecError::Unsupported("no playable tracks".into()));
        }
        let mut color = ColorInfo::REC709;
        let mut explicit_color = None;
        // display rotation from the track matrix (portrait phone video is stored landscape)
        let rotation = vtrack.and_then(|i| file.tracks[i].display_rotation()).unwrap_or(0);
        let video = vtrack.map(|i| {
            let t = &file.tracks[i];
            let entry = &t.entries[0];
            let vp = entry.video.clone().unwrap_or_default();
            let (w, h) = (if vp.width > 0 { vp.width as u32 } else { t.width }, if vp.height > 0 { vp.height as u32 } else { t.height });
            // frame rate from the median sample duration
            let mut durs: Vec<u32> = t.samples.iter().take(240).map(|s| s.duration).collect();
            durs.sort_unstable();
            let d = durs.get(durs.len() / 2).copied().unwrap_or(1).max(1);
            let rate = FrameRate::from_f64(t.timescale as f64 / d as f64);
            color = color_from(entry, w, h);
            if entry
                .video
                .as_ref()
                .is_some_and(|v| matches!(v.color, Some(filmcraft_isobmff::ColorInfo::Nclx { .. } | filmcraft_isobmff::ColorInfo::Nclc { .. })))
            {
                explicit_color = Some(color);
            }
            let bitrate = (t.samples.iter().map(|s| s.size as u64).sum::<u64>() * 8 * t.timescale as u64).checked_div(t.duration);
            let mut info = VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: rate,
                par: vp.pixel_aspect.unwrap_or((1, 1)),
                codec: codec_label(&entry.codec),
                pixel_format: pixfmt_label(&entry.codec),
                color,
                has_alpha: matches!(&entry.codec, CodecConfig::ProRes { fourcc } if fourcc.0 == *b"ap4h" || fourcc.0 == *b"ap4x"),
                bitrate,
            };
            if rotation % 2 == 1 {
                (info.width, info.height) = (info.height, info.width);
                info.par = (info.par.1, info.par.0);
            }
            if matches!(entry.codec, CodecConfig::Dnx { .. }) {
                // the sample entry doesn't say which VC-3 compression ID it is: read the first frame header
                let s0 = &t.samples[0];
                if let Some(h) = filmcraft_media::reader::read_range(&*bytes.0, s0.offset, s0.size as usize).ok().and_then(|d| filmcraft_dnx::probe(&d).ok()) {
                    info.codec = filmcraft_dnx::cid_name(h.cid);
                    let sub = match h.chroma {
                        filmcraft_dnx::ChromaFormat::Yuv420 => "YUV 4:2:0",
                        filmcraft_dnx::ChromaFormat::Yuv422 => "YUV 4:2:2",
                        filmcraft_dnx::ChromaFormat::Yuv444 => "4:4:4",
                    };
                    info.pixel_format = format!("{sub} {}-bit", h.bit_depth);
                    info.has_alpha = h.alpha;
                }
            }
            info
        });
        let audio = atrack.map(|i| {
            let t = &file.tracks[i];
            let entry = &t.entries[0];
            let ap = entry.audio.clone().unwrap_or_default();
            let (rate, ch, bits) = match &entry.codec {
                CodecConfig::Aac(a) => (
                    if a.sample_rate > 0 { a.sample_rate } else { ap.sample_rate as u32 },
                    if a.channel_config > 0 { a.channel_config as u32 } else { ap.channels },
                    None,
                ),
                CodecConfig::Pcm(p) => (p.sample_rate as u32, p.channels, Some(p.bits as u32)),
                CodecConfig::Opus(o) => (crate::audio::OPUS_RATE, (o.output_channels as u32).max(1), None),
                _ => (if ap.sample_rate > 0.0 { ap.sample_rate as u32 } else { t.timescale }, ap.channels.max(1), None),
            };
            AudioStreamInfo { sample_rate: rate.max(1), channels: ch.max(1), codec: codec_label(&entry.codec), bits_per_sample: bits }
        });
        let duration = {
            let v = vtrack.map(|i| {
                let t = &file.tracks[i];
                Tick::from_rational(t.duration as i64, 1, t.timescale as i64)
            });
            let a = atrack.map(|i| {
                let t = &file.tracks[i];
                Tick::from_rational(t.duration as i64, 1, t.timescale as i64)
            });
            v.or(a).unwrap_or_default()
        };
        let start_timecode = file.tracks.iter().find_map(|t| match t.codec() {
            Some(CodecConfig::Timecode(tc)) => tc.start_frame.map(|f| f as i64),
            _ => None,
        });
        let info = MediaInfo {
            name: name.to_string(),
            kind: if video.is_some() { MediaKind::Movie } else { MediaKind::AudioOnly },
            duration,
            video,
            audio,
            container: if file.is_quicktime { "QuickTime".into() } else { "MPEG-4".into() },
            start_timecode,
            file_size: Some(bytes.0.len()),
        };
        let audio_starts = atrack
            .map(|i| {
                let t = &file.tracks[i];
                let mut acc = 0i64;
                t.samples
                    .iter()
                    .map(|s| {
                        let st = acc;
                        acc += s.duration as i64;
                        st
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (audio_offset, audio_preroll) = atrack
            .map(|i| {
                let t = &file.tracks[i];
                let ts = t.timescale.max(1) as i64;
                match &t.entries[0].codec {
                    CodecConfig::Opus(o) => {
                        let off = if t.edit_offset != 0 { t.edit_offset } else { -(o.pre_skip as i64) * ts / 48_000 };
                        (off, (crate::audio::OPUS_PRE_ROLL as u64 * ts as u64).div_ceil(48_000) as i64)
                    }
                    _ => (t.edit_offset, 0),
                }
            })
            .unwrap_or((0, 0));
        Ok(Self {
            info,
            bytes,
            file,
            vtrack,
            atrack,
            video: GopCache::new(explicit_color).with_rotation(rotation),
            audio: Mutex::new(AudioState { decoder: None, packets: HashMap::new(), order: Vec::new(), last_decoded: None }),
            audio_starts,
            audio_offset,
            audio_preroll,
        })
    }

    fn read(&self, track: usize, i: usize) -> crate::Result<Vec<u8>> {
        self.file.read_sample(&self.bytes, track, i).map_err(|e| CodecError::Container(e.to_string()))
    }

    fn video_at(&self, t: Tick) -> crate::Result<Arc<VideoFrame>> {
        let ti = self.vtrack.ok_or_else(|| CodecError::Unsupported("no video".into()))?;
        let ts = self.file.tracks[ti].timescale as i64;
        let late = filmcraft_media::cancel::catch_up().map(|m| (t - m).to_rational_floor(1, ts));
        self.video.frame_late(&Mp4Video { src: self, track: ti }, t.to_rational_floor(1, ts), late)
    }

    fn audio_packet(&self, st: &mut AudioState, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
        if let Some(p) = st.packets.get(&i) {
            return Ok(p.clone());
        }
        let ti = self.atrack.expect("audio");
        let track = &self.file.tracks[ti];
        let entry = &track.entries[0];
        let data = self.read(ti, i)?;
        let decoded = match &entry.codec {
            CodecConfig::Pcm(p) => decode_pcm(&data, p),
            c => {
                if st.decoder.is_none() {
                    st.decoder = Some(PacketDecoder::for_isobmff(c, self.info.audio.as_ref().map_or(48_000, |a| a.sample_rate))?);
                }
                // Non-sequential access: reset and prime with the preceding packets (codec pre-roll:
                // one packet, or `OPUS_PRE_ROLL` worth for Opus).
                if st.last_decoded.is_none_or(|l| l + 1 != i) {
                    let d = st.decoder.as_mut().expect("decoder");
                    d.reset();
                    let from = if self.audio_preroll > 0 {
                        self.audio_starts.partition_point(|&x| x <= self.audio_starts[i] - self.audio_preroll).saturating_sub(1)
                    } else {
                        i.saturating_sub(1)
                    };
                    for j in from..i {
                        if let Ok(prev) = self.read(ti, j) {
                            let _ = d.decode(&prev, 0);
                        }
                    }
                }
                let r = st.decoder.as_mut().expect("decoder").decode(&data, self.audio_starts[i].max(0) as u64);
                st.last_decoded = Some(i);
                match r {
                    Ok(v) => v,
                    Err(_) => vec![vec![0.0; track.samples[i].duration as usize]; self.info.audio.as_ref().map_or(2, |a| a.channels as usize)],
                }
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

/// The MP4 video track as a [`VideoSamples`] table.
struct Mp4Video<'a> {
    src: &'a Mp4Source,
    track: usize,
}

impl VideoSamples for Mp4Video<'_> {
    fn count(&self) -> usize {
        self.src.file.tracks[self.track].samples.len()
    }
    fn pts(&self, i: usize) -> i64 {
        self.src.file.tracks[self.track].samples[i].pts
    }
    fn sync_before(&self, i: usize) -> usize {
        self.src.file.tracks[self.track].sync_sample_before(i)
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        self.src.file.tracks[self.track].sample_at_presentation_time(t)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        self.src.read(self.track, i)
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        make_video_decoder(&self.src.file.tracks[self.track].entries[0])
    }
}

impl MediaSource for Mp4Source {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let f = self.video_at(req.time.max(Tick::ZERO))?;
        Ok(f)
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let ti = self.atrack.ok_or(MediaError::NoStream("audio"))?;
        let track = &self.file.tracks[ti];
        let ainfo = self.info.audio.as_ref().ok_or(MediaError::NoStream("audio"))?;
        let src_rate = ainfo.sample_rate;
        let ch = ainfo.channels.max(1) as usize;
        // Map the requested window to source samples (edit list offset applied: presentation = pts + edit_offset).
        let ratio = src_rate as f64 / sample_rate as f64;
        let s0 = (start as f64 * ratio).floor() as i64 - self.audio_offset * src_rate as i64 / track.timescale.max(1) as i64;
        let need = (frames as f64 * ratio).ceil() as i64 + 2;
        // Samples-per-unit: the track timescale is usually the sample rate for audio.
        let unit = src_rate as f64 / track.timescale.max(1) as f64;
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        let mut st = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        let first = self.audio_starts.partition_point(|&x| (x as f64 * unit) as i64 <= s0.max(0)).saturating_sub(1);
        let mut i = first;
        while i < track.samples.len() {
            let pk_start = (self.audio_starts[i] as f64 * unit) as i64;
            if pk_start >= s0 + need {
                break;
            }
            let pk = self.audio_packet(&mut st, i)?;
            for (c, dst) in src.iter_mut().enumerate() {
                let Some(chan) = pk.get(c.min(pk.len().saturating_sub(1))) else { continue };
                for (k, v) in chan.iter().enumerate() {
                    let pos = pk_start + k as i64 - s0;
                    if pos >= 0 && (pos as usize) < dst.len() {
                        dst[pos as usize] = *v;
                    }
                }
            }
            i += 1;
        }
        drop(st);
        if src_rate == sample_rate {
            let mut out = AudioBuffer::silence(sample_rate, ch, frames);
            for c in 0..ch {
                out.channels[c]
                    .copy_from_slice(&src[c][..frames.min(src[c].len())].iter().copied().chain(std::iter::repeat(0.0)).take(frames).collect::<Vec<_>>());
            }
            return Ok(out);
        }
        // resample the local window
        let frac = s0 as f64 - start as f64 * ratio;
        let mut out = AudioBuffer::silence(sample_rate, ch, frames);
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

fn codec_label(c: &CodecConfig) -> String {
    match c {
        CodecConfig::Avc(a) => format!(
            "H.264 ({})",
            match a.profile {
                66 => "Baseline",
                77 => "Main",
                100 => "High",
                110 => "High 10",
                122 => "High 4:2:2",
                244 => "High 4:4:4",
                _ => "AVC",
            }
        ),
        CodecConfig::Hevc(_) => "HEVC".into(),
        CodecConfig::Vp9(c) => format!("VP9 (Profile {})", c.profile),
        CodecConfig::Av1(c) => format!("AV1 ({} Profile)", ["Main", "High", "Professional"].get(c.seq_profile as usize).unwrap_or(&"Main")),
        CodecConfig::ProRes { fourcc } => match &fourcc.0 {
            b"apco" => "Apple ProRes 422 Proxy".into(),
            b"apcs" => "Apple ProRes 422 LT".into(),
            b"apcn" => "Apple ProRes 422".into(),
            b"apch" => "Apple ProRes 422 HQ".into(),
            b"ap4h" => "Apple ProRes 4444".into(),
            b"ap4x" => "Apple ProRes 4444 XQ".into(),
            _ => "Apple ProRes".into(),
        },
        CodecConfig::Aac(a) => {
            if a.object_type == 5 || a.object_type == 29 {
                "HE-AAC".into()
            } else {
                "AAC".into()
            }
        }
        CodecConfig::Pcm(p) => format!("PCM {}-bit{}", p.bits, if p.float { " float" } else { "" }),
        CodecConfig::Opus(_) => "Opus".into(),
        CodecConfig::Dnx { fourcc } if fourcc.0 == *b"AVdh" => "Avid DNxHR".into(),
        CodecConfig::Dnx { .. } => "Avid DNxHD".into(),
        other => other.name().to_string(),
    }
}

fn pixfmt_label(c: &CodecConfig) -> String {
    match c {
        CodecConfig::Avc(a) => match a.profile {
            110 => "YUV 4:2:0 10-bit".into(),
            122 => "YUV 4:2:2".into(),
            244 => "YUV 4:4:4".into(),
            _ => "YUV 4:2:0 8-bit".into(),
        },
        CodecConfig::Vp9(c) => {
            let sub = match c.chroma_subsampling {
                2 => "4:2:2",
                3 => "4:4:4",
                _ => "4:2:0",
            };
            format!("YUV {sub} {}-bit", c.bit_depth.max(8))
        }
        CodecConfig::ProRes { fourcc } if fourcc.0[2] == b'4' => "YUVA 4:4:4 12-bit".into(),
        CodecConfig::ProRes { .. } => "YUV 4:2:2 10-bit".into(),
        CodecConfig::Jpeg { .. } => "YUV 4:2:x 8-bit".into(),
        _ => String::new(),
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes) {
        return None;
    }
    Some(Mp4Source::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

/// [`filmcraft_media::ReaderOpener`] for MP4/MOV.
pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(head) {
        return None;
    }
    Some(Mp4Source::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    use filmcraft_media::FrameRequest;

    /// A one-frame 64×32 ProRes MOV (bright left half, dark right half) whose `tkhd` matrix is
    /// replaced by `matrix` — built here, no media files.
    fn rotated_mov(matrix: [i32; 9]) -> Arc<[u8]> {
        let (w, h) = (64u32, 32u32);
        let mut fr = filmcraft_prores::Frame::new(w, h, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
        for (i, y) in fr.y.iter_mut().enumerate() {
            *y = if (i as u32 % w) < w / 2 { 800 } else { 100 };
        }
        let data = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, w, h).encode(&fr).expect("encode");
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
        let t = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), w as u16, h as u16), 25)).expect("track");
        mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
        let mut b = mux.finish().expect("finish").into_inner();
        // tkhd (ISO/IEC 14496-12 §8.3.2): version/flags, then 20 (v0) or 32 (v1) bytes of
        // times/id/duration, 8 reserved, layer, alternate_group, volume, reserved, matrix
        let k = b.windows(4).position(|x| x == b"tkhd").expect("tkhd") + 4;
        let m = k + 4 + if b[k] == 1 { 32 } else { 20 } + 16;
        for (j, v) in matrix.iter().enumerate() {
            b[m + j * 4..m + j * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        b.into()
    }

    /// Mean luma of the top and bottom halves of the decoded frame, and its size.
    fn halves(src: &Mp4Source) -> (u32, u32, u8, u8) {
        let f = src.video_frame(FrameRequest::full(Tick::ZERO)).expect("frame");
        let l = f.luma8();
        let half = l.len() / 2;
        let mean = |s: &[u8]| (s.iter().map(|&v| v as u32).sum::<u32>() / s.len() as u32) as u8;
        (f.width, f.height, mean(&l[..half]), mean(&l[half..]))
    }

    #[test]
    fn display_matrix_rotates_frames_and_reported_size() {
        const ONE: i32 = 0x10000;
        const W: i32 = 0x4000_0000;
        // identity: landscape 64x32, top and bottom alike
        let s = Mp4Source::open("id.mov", rotated_mov([ONE, 0, 0, 0, ONE, 0, 0, 0, W])).expect("open");
        let v = s.info().video.as_ref().expect("video");
        assert_eq!((v.width, v.height), (64, 32));
        let (w, h, top, bottom) = halves(&s);
        assert_eq!((w, h), (64, 32));
        assert!(top.abs_diff(bottom) < 4);

        // 90° clockwise (what an iPhone writes for portrait): the left half ends up on top
        let s = Mp4Source::open("cw.mov", rotated_mov([0, ONE, 0, -ONE, 0, 0, 32 * ONE, 0, W])).expect("open");
        let v = s.info().video.as_ref().expect("video");
        assert_eq!((v.width, v.height), (32, 64));
        let (w, h, top, bottom) = halves(&s);
        assert_eq!((w, h), (32, 64));
        assert!(top > bottom + 100, "top {top} bottom {bottom}");

        // 270° clockwise: the left half ends up at the bottom
        let s = Mp4Source::open("ccw.mov", rotated_mov([0, -ONE, 0, ONE, 0, 0, 0, 64 * ONE, W])).expect("open");
        let (w, h, top, bottom) = halves(&s);
        assert_eq!((w, h), (32, 64));
        assert!(bottom > top + 100, "top {top} bottom {bottom}");

        // 180°: still landscape
        let s = Mp4Source::open("180.mov", rotated_mov([-ONE, 0, 0, 0, -ONE, 0, 64 * ONE, 32 * ONE, W])).expect("open");
        assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((64, 32)));
    }
}
