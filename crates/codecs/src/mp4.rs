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

/// Most bytes read from one AAC access unit when probing for HE-AAC at open (8 channels × 6144 bits).
const MAX_AAC_PROBE_UNIT: usize = 8 * 6144 / 8;

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

/// HDR static metadata from the sample entry's `mdcv` / `clli` boxes (0 = unknown).
fn hdr_metadata(md: Option<&filmcraft_isobmff::MasteringDisplay>, cll: Option<(u16, u16)>) -> Option<filmcraft_color::HdrMetadata> {
    if md.is_none() && cll.is_none() {
        return None;
    }
    let nz = |v: f32| (v > 0.0).then_some(v);
    Some(filmcraft_color::HdrMetadata {
        mastering_max_nits: md.and_then(|m| nz(m.max_nits() as f32)),
        mastering_min_nits: md.map(|m| m.min_luminance as f32 / 10_000.0),
        max_cll: cll.and_then(|c| nz(c.0 as f32)),
        max_fall: cll.and_then(|c| nz(c.1 as f32)),
    })
}

fn color_from(entry: &filmcraft_isobmff::SampleEntry, w: u32, h: u32) -> ColorInfo {
    let mut c = ColorInfo { matrix: filmcraft_frame::default_matrix(w, h), transfer: Transfer::Bt709, primaries: Primaries::Bt709, range: Range::Limited };
    // VP9 and APV carry their colour description in vpcC / apvC (used when there is no colr box).
    let vpc = match &entry.codec {
        CodecConfig::Vp9(v) => Some(filmcraft_isobmff::ColorInfo::Nclx {
            primaries: v.colour_primaries as u16,
            transfer: v.transfer_characteristics as u16,
            matrix: v.matrix_coefficients as u16,
            full_range: v.full_range,
        }),
        CodecConfig::Apv(a) if a.color_description_present => Some(filmcraft_isobmff::ColorInfo::Nclx {
            primaries: a.color_primaries as u16,
            transfer: a.transfer_characteristics as u16,
            matrix: a.matrix_coefficients as u16,
            full_range: a.full_range,
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
        // A track needs samples, a sample description and a timescale to be playable (damaged
        // files can lack them: indexing `entries[0]` or dividing by the timescale used to panic).
        let playable = |t: &&filmcraft_isobmff::Track, kind| t.kind == kind && !t.samples.is_empty() && !t.entries.is_empty() && t.timescale > 0;
        let vtrack = file.tracks.iter().position(|t| playable(&t, TrackKind::Video));
        let atrack = file.tracks.iter().position(|t| playable(&t, TrackKind::Audio));
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
            // A rate that rounds to zero (one sample per huge duration) can't be divided by.
            let rate = if rate.num > 0 && rate.den > 0 { rate } else { FrameRate::default() };
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
                // ProRes 4444 alpha comes from its first frame header (`open_reader`)
                has_alpha: matches!(&entry.codec, CodecConfig::Apv(a) if a.chroma_format_idc == 4),
                bitrate,
                hdr: entry.video.as_ref().and_then(|v| hdr_metadata(v.mastering_display.as_ref(), v.content_light)),
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
            #[cfg(feature = "prores")]
            if matches!(entry.codec, CodecConfig::ProRes { .. }) {
                // a 4444 fourcc doesn't mean the frames carry alpha (alpha_channel_type does): read the first frame header
                let s0 = &t.samples[0];
                if let Some(h) = filmcraft_media::reader::read_range(&*bytes.0, s0.offset, s0.size as usize).ok().and_then(|d| filmcraft_prores::probe(&d).ok())
                {
                    info.has_alpha = h.alpha != filmcraft_prores::AlphaType::None;
                    let sub = match h.chroma {
                        filmcraft_prores::ChromaFormat::Yuv422 => "4:2:2 10-bit",
                        filmcraft_prores::ChromaFormat::Yuv444 => "4:4:4 12-bit",
                    };
                    info.pixel_format = format!("{} {sub}", if info.has_alpha { "YUVA" } else { "YUV" });
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
                    {
                        // HE-AAC plays at twice the core rate the AudioSpecificConfig starts with
                        // an AAC access unit is at most 6144 bits per channel: a hostile `stsz` can't make us read more
                        let units = t
                            .samples
                            .iter()
                            .take(8)
                            .filter_map(|x| filmcraft_media::reader::read_range(&*bytes.0, x.offset, (x.size as usize).min(MAX_AAC_PROBE_UNIT)).ok());
                        let units: Vec<Vec<u8>> = units.collect();
                        match crate::audio::aac_output_rate(&a.asc, units.iter().map(Vec::as_slice)) {
                            Some(r) => r,
                            None if a.sample_rate > 0 => a.sample_rate,
                            None => ap.sample_rate as u32,
                        }
                    },
                    if a.channel_config > 0 { a.channel_config as u32 } else { ap.channels },
                    None,
                ),
                CodecConfig::Pcm(p) => (p.sample_rate as u32, p.channels, Some(p.bits as u32)),
                CodecConfig::Opus(o) => (crate::audio::OPUS_RATE, (o.output_channels as u32).max(1), None),
                _ => (if ap.sample_rate > 0.0 { ap.sample_rate as u32 } else { t.timescale }, ap.channels.max(1), None),
            };
            AudioStreamInfo { sample_rate: rate.max(1), channels: ch.max(1), codec: codec_label(&entry.codec), bits_per_sample: bits }
        });
        let duration = vtrack.or(atrack).and_then(|i| file.tracks.get(i)).map(|t| presentation_duration(&file, t)).unwrap_or_default();
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
        // Nearest, not floor: with a coarse timescale (1000) sample times are rounded, and a
        // floored lookup lands one unit short of every frame whose time was rounded up.
        let late = filmcraft_media::cancel::catch_up().map(|m| (t - m).to_rational_round(1, ts));
        self.video.frame_late(&Mp4Video { src: self, track: ti }, t.to_rational_round(1, ts), late)
    }

    fn audio_packet(&self, st: &mut AudioState, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
        if let Some(p) = st.packets.get(&i) {
            return Ok(p.clone());
        }
        let ti = self.atrack.ok_or_else(|| CodecError::Unsupported("no audio track".into()))?;
        let track = &self.file.tracks[ti];
        let entry = track.entries.first().ok_or_else(|| CodecError::Container("audio track without a sample description".into()))?;
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
                    let d = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?;
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
                let r = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?.decode(&data, self.audio_starts[i].max(0) as u64);
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
        let p0 = (start as f64 * ratio).floor() as i64;
        // (in i128: a hostile edit list can make the offset large enough to overflow i64)
        let offset = i128::from(self.audio_offset) * i128::from(src_rate) / i128::from(track.timescale.max(1));
        let s0 = i64::try_from(i128::from(p0) - offset).unwrap_or(if offset > 0 { i64::MIN } else { i64::MAX });
        let need = (frames as f64 * ratio).ceil() as i64 + 2;
        // Samples-per-unit: the track timescale is usually the sample rate for audio.
        let unit = src_rate as f64 / track.timescale.max(1) as f64;
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        let mut st = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        let first = self.audio_starts.partition_point(|&x| (x as f64 * unit) as i64 <= s0.max(0)).saturating_sub(1);
        let mut i = first;
        while i < track.samples.len() {
            let pk_start = (self.audio_starts[i] as f64 * unit) as i64;
            if pk_start >= s0.saturating_add(need) {
                break;
            }
            let pk = self.audio_packet(&mut st, i)?;
            for (c, dst) in src.iter_mut().enumerate() {
                let Some(chan) = pk.get(c.min(pk.len().saturating_sub(1))) else { continue };
                for (k, v) in chan.iter().enumerate() {
                    let pos = pk_start.saturating_add(k as i64).saturating_sub(s0);
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
        // Resample the local window. Its first sample is presentation sample `p0`; the edit list
        // offset is already in `s0` and must not shift the read position a second time.
        let frac = p0 as f64 - start as f64 * ratio;
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
        CodecConfig::Apv(a) => match filmcraft_apv::Profile::from_idc(a.profile_idc) {
            Some(p) => p.name().into(),
            None => "APV".into(),
        },
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

/// "YUV 4:2:2 10-bit" from an H.264 / HEVC `chroma_format_idc` and bit depth.
fn yuv_label(chroma_format_idc: u32, bits: u32) -> String {
    let sub = match chroma_format_idc {
        0 => "4:0:0",
        1 => "4:2:0",
        2 => "4:2:2",
        _ => "4:4:4",
    };
    format!("YUV {sub} {bits}-bit")
}

/// `chroma_format_idc` and luma bit depth of an H.264 track: from the `avcC` High-profile extension
/// (ISO/IEC 14496-15 §5.3.3), else from its first SPS. The profile alone can't say (High 4:2:2 is
/// 8 or 10-bit, High 10 can be 8-bit).
fn avc_format(a: &filmcraft_isobmff::AvcConfig) -> Option<(u32, u32)> {
    // reserved bits set: '111111' chroma_format, '11111' bit_depth_luma_minus8 (else not an extension)
    if !matches!(a.profile, 66 | 77 | 88)
        && let &[chroma, depth, ..] = a.ext.as_slice()
        && chroma & 0xFC == 0xFC
        && depth & 0xF8 == 0xF8
    {
        return Some(((chroma & 3) as u32, (depth & 7) as u32 + 8));
    }
    #[cfg(feature = "h264")]
    {
        let sps = filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(a.sps.first()?.get(1..)?)).ok()?;
        Some((sps.chroma_format_idc, sps.bit_depth_luma))
    }
    #[cfg(not(feature = "h264"))]
    None
}

fn pixfmt_label(c: &CodecConfig) -> String {
    match c {
        CodecConfig::Avc(a) => avc_format(a).map(|(chroma, bits)| yuv_label(chroma, bits)).unwrap_or_default(),
        CodecConfig::Hevc(h) => yuv_label(h.chroma_format_idc as u32, h.bit_depth_luma as u32),
        CodecConfig::Vp9(c) => {
            let sub = match c.chroma_subsampling {
                2 => "4:2:2",
                3 => "4:4:4",
                _ => "4:2:0",
            };
            format!("YUV {sub} {}-bit", c.bit_depth.max(8))
        }
        CodecConfig::Apv(a) => {
            let chroma = filmcraft_apv::ChromaFormat::from_idc(a.chroma_format_idc).unwrap_or(filmcraft_apv::ChromaFormat::Yuv422);
            crate::apv::apv_pixfmt_label(chroma, a.bit_depth_minus8.saturating_add(8))
        }
        // 4444 until the first frame header says whether it codes alpha (`open_reader`)
        CodecConfig::ProRes { fourcc } if fourcc.0[2] == b'4' => "YUV 4:4:4 12-bit".into(),
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

/// How long a track plays. With an edit list that is the sum of its edits (ISO/IEC 14496-12
/// §8.6.6, movie timescale), as `tkhd` records it. The media duration (`mdhd`) runs on the decode
/// timeline: it is longer than the presentation by the B-frame delay an edit skips (`media_time`, one
/// extra frame on every ffmpeg / x264 MP4 with B-frames) and shorter by a leading empty edit (the last
/// frame of a delayed video track was never shown). Fragmented files, and edit lists that add up to
/// nothing or to more than a [`Tick`] holds, keep the media duration.
fn presentation_duration(file: &Mp4File, t: &filmcraft_isobmff::Track) -> Tick {
    let media = Tick::from_rational(t.duration as i64, 1, t.timescale as i64);
    if file.fragmented || t.edits.is_empty() || file.timescale == 0 {
        return media;
    }
    let edits = t.edits.iter().fold(0u128, |a, e| a.saturating_add(e.segment_duration as u128));
    let ticks = edits.saturating_mul(filmcraft_time::TICKS_PER_SECOND as u128) / file.timescale as u128;
    match i64::try_from(ticks) {
        Ok(t) if t > 0 => Tick(t),
        _ => media,
    }
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

    /// A four-frame 25 fps ProRes MOV (64×32) whose video track carries `edits`.
    fn mov_with_edits(edits: Vec<filmcraft_isobmff::Edit>) -> Arc<[u8]> {
        let fr = filmcraft_prores::Frame::new(64, 32, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
        let data = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, 64, 32).encode(&fr).expect("encode");
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
        let mut cfg = TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), 64, 32), 25);
        cfg.edits = edits;
        let t = mux.add_track(cfg).expect("track");
        for _ in 0..4 {
            mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
        }
        mux.finish().expect("finish").into_inner().into()
    }

    /// A clip lasts as long as its edit list presents it, not as long as its media (`mdhd`) runs.
    /// Every ffmpeg / x264 MP4 with B-frames skips its reorder delay with an edit and imported one
    /// frame too long; a video track delayed by an empty edit lost its last frame.
    #[test]
    fn duration_follows_the_edit_list() {
        use filmcraft_isobmff::Edit;
        let frames = |s: &Mp4Source| s.info().duration.to_rational_floor(1, 25);
        // no edit list: the media duration
        assert_eq!(frames(&Mp4Source::open("plain.mov", mov_with_edits(Vec::new())).expect("open")), 4);
        // skip the first frame (B-frame delay style): three frames play (movie timescale 1000 ms)
        let skip = vec![Edit { segment_duration: 120, media_time: 1, media_rate: 0x10000 }];
        assert_eq!(frames(&Mp4Source::open("skip.mov", mov_with_edits(skip)).expect("open")), 3);
        // a one-frame empty edit, then all four frames: five frames, and the last one is there
        let delayed =
            vec![Edit { segment_duration: 40, media_time: -1, media_rate: 0x10000 }, Edit { segment_duration: 160, media_time: 0, media_rate: 0x10000 }];
        let s = Mp4Source::open("delayed.mov", mov_with_edits(delayed)).expect("open");
        assert_eq!(frames(&s), 5);
        assert!(s.video_frame(FrameRequest::full(Tick::from_rational(4, 1, 25))).is_ok());
        // hostile segment durations fall back to the media duration instead of wrapping
        let huge = vec![Edit { segment_duration: u64::MAX, media_time: 0, media_rate: 0x10000 }];
        assert_eq!(frames(&Mp4Source::open("huge.mov", mov_with_edits(huge)).expect("open")), 4);
        let zero = vec![Edit { segment_duration: 0, media_time: 0, media_rate: 0x10000 }];
        assert_eq!(frames(&Mp4Source::open("zero.mov", mov_with_edits(zero)).expect("open")), 4);
    }

    /// Overwrite the big-endian u32 `skip` bytes after the first `fourcc` box type.
    fn patch_u32(b: &mut [u8], fourcc: &[u8; 4], skip: usize, v: u32) {
        let k = b.windows(4).position(|x| x == fourcc).expect("box") + 4 + skip;
        b[k..k + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// Damaged MOVs found by mutation fuzzing panicked on import: a track with timescale 0
    /// ("attempt to divide by zero" in the duration) and one whose sample description list is
    /// empty ("index out of bounds" on `entries[0]`). Both must open as an error, or as media
    /// that answers requests without panicking.
    #[test]
    fn damaged_track_headers_never_panic() {
        let identity = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];
        // mdhd v0: version/flags, creation, modification, then timescale
        let mut zero_timescale = rotated_mov(identity).to_vec();
        patch_u32(&mut zero_timescale, b"mdhd", 12, 0);
        // stsd: version/flags, then entry_count
        let mut no_entries = rotated_mov(identity).to_vec();
        patch_u32(&mut no_entries, b"stsd", 4, 0);
        for (name, b) in [("ts0.mov", zero_timescale), ("stsd0.mov", no_entries)] {
            if let Ok(s) = Mp4Source::open(name, b.into()) {
                let _ = s.video_frame(FrameRequest::full(Tick::ZERO));
                let _ = s.audio(0, 1024, 48_000);
            }
        }
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

    #[test]
    fn mirrored_or_scaled_matrices_are_not_mistaken_for_rotations() {
        const ONE: i32 = 0x10000;
        const W: i32 = 0x4000_0000;
        // horizontal mirror: not a pure rotation, so the frame is left as stored
        let s = Mp4Source::open("mirror.mov", rotated_mov([-ONE, 0, 0, 0, ONE, 0, 64 * ONE, 0, W])).expect("open");
        let (w, h, top, bottom) = halves(&s);
        assert_eq!((w, h), (64, 32));
        assert!(top.abs_diff(bottom) < 4);
        // a 2× scaled identity is still upright (only the signs decide the rotation)
        let s = Mp4Source::open("scaled.mov", rotated_mov([2 * ONE, 0, 0, 0, 2 * ONE, 0, 0, 0, W])).expect("open");
        assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((64, 32)));
        // a scaled quarter turn is still a quarter turn
        let s = Mp4Source::open("cw2.mov", rotated_mov([0, 2 * ONE, 0, -2 * ONE, 0, 0, 64 * ONE, 0, W])).expect("open");
        assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((32, 64)));
    }

    #[test]
    fn hevc_pixel_format_comes_from_hvcc() {
        for (chroma, bits, want) in [(1, 8, "YUV 4:2:0 8-bit"), (1, 10, "YUV 4:2:0 10-bit"), (2, 8, "YUV 4:2:2 8-bit"), (2, 10, "YUV 4:2:2 10-bit")] {
            let c = filmcraft_isobmff::HevcConfig { general_profile_idc: 1, chroma_format_idc: chroma, bit_depth_luma: bits, ..Default::default() };
            assert_eq!(pixfmt_label(&CodecConfig::Hevc(c)), want);
        }
    }

    /// A High-profile SPS NAL unit (ITU-T H.264 §7.3.2.1.1) with the given chroma format and bit depth, 16×16.
    fn high_sps(profile: u8, chroma: u32, bits: u32) -> Vec<u8> {
        let mut w = filmcraft_bitstream::BitWriter::new();
        w.write_bits(0x67, 8);
        w.write_bits(profile as u32, 8);
        w.write_bits(0, 8); // constraint flags
        w.write_bits(30, 8); // level
        w.write_ue(0); // seq_parameter_set_id
        w.write_ue(chroma);
        if chroma == 3 {
            w.write_bit(false); // separate_colour_plane_flag
        }
        w.write_ue(bits - 8); // luma
        w.write_ue(bits - 8); // chroma
        w.write_bits(0, 2); // no transform bypass, no scaling matrices
        w.write_ue(0); // log2_max_frame_num_minus4
        w.write_ue(2); // pic_order_cnt_type
        w.write_ue(1); // max_num_ref_frames
        w.write_bit(false); // gaps_in_frame_num_allowed
        w.write_ue(0); // pic_width_in_mbs_minus1
        w.write_ue(0); // pic_height_in_map_units_minus1
        w.write_bits(0b110, 3); // frame_mbs_only, direct_8x8_inference, no cropping
        w.write_bit(false); // no VUI
        w.rbsp_trailing();
        w.finish()
    }

    #[test]
    fn avc_pixel_format_comes_from_avcc_not_the_profile() {
        let avc =
            |profile: u8, sps: Vec<u8>, ext: Vec<u8>| CodecConfig::Avc(filmcraft_isobmff::AvcConfig { profile, sps: vec![sps], ext, ..Default::default() });
        // High 4:2:2 at 10 and 8 bits, from the avcC extension (chroma_format 2, bit_depth_luma_minus8)
        assert_eq!(pixfmt_label(&avc(122, high_sps(122, 2, 10), vec![0xFE, 0xFA, 0xFA, 0])), "YUV 4:2:2 10-bit");
        assert_eq!(pixfmt_label(&avc(122, high_sps(122, 2, 8), vec![0xFE, 0xF8, 0xF8, 0])), "YUV 4:2:2 8-bit");
        // no extension (or not a valid one): the SPS says
        assert_eq!(pixfmt_label(&avc(122, high_sps(122, 2, 10), Vec::new())), "YUV 4:2:2 10-bit");
        assert_eq!(pixfmt_label(&avc(110, high_sps(110, 1, 10), vec![0, 0, 0, 0])), "YUV 4:2:0 10-bit");
        assert_eq!(pixfmt_label(&avc(244, high_sps(244, 3, 10), Vec::new())), "YUV 4:4:4 10-bit");
        // an unreadable SPS and no extension: unknown, not a guess
        assert_eq!(pixfmt_label(&avc(122, vec![0x67, 122], Vec::new())), "");
    }

    /// A one-frame 32×16 ProRes MOV with the given profile, chroma format and fourcc.
    fn prores_mov(profile: filmcraft_prores::Profile, fourcc: &[u8; 4], chroma: filmcraft_prores::ChromaFormat, alpha: bool) -> Arc<[u8]> {
        let (w, h) = (32u32, 16u32);
        let fr = filmcraft_prores::Frame::new(w, h, chroma, if chroma == filmcraft_prores::ChromaFormat::Yuv444 { 12 } else { 10 }, alpha);
        let data = filmcraft_prores::Encoder::new(profile, w, h).encode(&fr).expect("encode");
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
        let t = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*fourcc), w as u16, h as u16), 25)).expect("track");
        mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
        mux.finish().expect("finish").into_inner().into()
    }

    /// `ap4h` / `ap4x` don't imply alpha: the frame header's alpha_channel_type does.
    #[test]
    fn prores_alpha_comes_from_the_frame_header() {
        use filmcraft_prores::{ChromaFormat, Profile};
        for (profile, fourcc, chroma, alpha, want) in [
            (Profile::P4444, b"ap4h", ChromaFormat::Yuv444, false, "YUV 4:4:4 12-bit"),
            (Profile::P4444, b"ap4h", ChromaFormat::Yuv444, true, "YUVA 4:4:4 12-bit"),
            (Profile::P4444Xq, b"ap4x", ChromaFormat::Yuv444, false, "YUV 4:4:4 12-bit"),
            (Profile::Hq, b"apch", ChromaFormat::Yuv422, false, "YUV 4:2:2 10-bit"),
        ] {
            let s = Mp4Source::open("p.mov", prores_mov(profile, fourcc, chroma, alpha)).expect("open");
            let v = s.info().video.as_ref().expect("video");
            assert_eq!((v.pixel_format.as_str(), v.has_alpha), (want, alpha), "{}", String::from_utf8_lossy(fourcc));
        }
    }
}

#[cfg(test)]
mod he_aac_tests {
    use filmcraft_isobmff::{Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    use filmcraft_media::MediaSource;

    /// #108: an HE-AAC MP4 (explicit SBR signalling, core 22.05 kHz, output 44.1 kHz, timescale
    /// 44.1 kHz) built here from our AAC-LC encoder: the stream has no SBR data, which only the
    /// high band would need. It was reported at the 22.05 kHz core rate, so sequences made from
    /// such a clip were created at 22.05 kHz.
    #[test]
    fn he_aac_plays_at_the_output_rate() {
        let core = 22_050u32;
        let n = core as usize * 2;
        let tone: Vec<f32> = (0..n).map(|i| 0.25 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / core as f32).sin()).collect();
        let mut enc = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(core, 2, 64_000)).unwrap();
        let mut aus = enc.encode(&[&tone, &tone]);
        aus.extend(enc.flush());
        // AOT 5, core 22.05 kHz, stereo, extension 44.1 kHz, core AOT 2
        let asc = vec![0x2B, 0x92, 0x08, 0x00];
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
        let t = mux.add_track(TrackConfig::new(SampleEntry::aac(asc, 2, 44_100), 44_100)).unwrap();
        for au in &aus {
            mux.write_sample(t, WriteSample { data: au, duration: 2048, composition_offset: 0, is_sync: true }).unwrap();
        }
        let b = mux.finish().unwrap().into_inner();
        let src = super::Mp4Source::open("he.mp4", b.into()).unwrap();
        assert_eq!(src.info().audio.as_ref().unwrap().sample_rate, 44_100);
        // one second from 0.5 s at 44.1 kHz: the 1 kHz tone at its level (not silence, not shifted)
        let buf = src.audio(22_050, 44_100, 44_100).unwrap();
        let x = &buf.channels[0];
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((0.2..0.3).contains(&peak), "peak {peak}");
        let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((995..=1005).contains(&crossings), "{crossings} Hz");
    }

    /// A hostile edit list (#66): an AAC track whose edit puts it absurdly far off, read at another
    /// rate, scaled the offset to the source rate in i64 and overflowed. It reads as silence now.
    #[test]
    fn hostile_audio_edit_offset_never_overflows() {
        use filmcraft_isobmff::Edit;
        let rate = 32_000u32;
        let tone: Vec<f32> = (0..rate as usize / 2).map(|i| 0.25 * (i as f32 * 0.05).sin()).collect();
        let mut enc = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(rate, 1, 64_000)).unwrap();
        let mut aus = enc.encode(&[&tone]);
        aus.extend(enc.flush());
        // AOT 2 (AAC-LC), 32 kHz (index 5), mono
        let asc = vec![0x12, 0x88];
        for media_time in [i64::MAX, i64::MAX / 3, i64::MIN / 3] {
            let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
            let mut cfg = TrackConfig::new(SampleEntry::aac(asc.clone(), 1, rate), rate);
            cfg.edits = vec![Edit { segment_duration: 500, media_time, media_rate: 0x10000 }];
            let t = mux.add_track(cfg).unwrap();
            for au in &aus {
                mux.write_sample(t, WriteSample { data: au, duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
            }
            let src = super::Mp4Source::open("hostile.mp4", mux.finish().unwrap().into_inner().into()).unwrap();
            let buf = src.audio(0, 4800, 48_000).unwrap();
            assert!(buf.channels.iter().flatten().all(|v| v.is_finite() && v.abs() <= 1.0), "media_time {media_time}");
        }
    }
}
