//! MP4/MOV media source with GOP-aware random access (see [`crate::gop`]).
//!
//! Opus (`Opus` sample entry + `dOps`): always 48 kHz output. Pre-skip is removed by the edit list
//! (`media_time` = pre-skip), or applied from `dOps` when a file has no edit list; random access
//! decodes [`crate::audio::OPUS_PRE_ROLL`] of preceding packets first (≥ the 80 ms `roll` distance).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filmcraft_color::ColorInfo;
use filmcraft_frame::{AudioBuffer, Region, VideoFrame};
use filmcraft_isobmff::{ByteSource, CleanAperture, CodecConfig, Mp4File, Sample, TrackKind};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::audio::{PacketDecoder, decode_pcm};
use crate::gop::{GopCache, VideoSamples};
use crate::video::VideoDecoder;
use crate::{CodecError, make_video_decoder};

/// Most bytes read from one AAC access unit when probing for HE-AAC at open (8 channels × 6144 bits).
const MAX_AAC_PROBE_UNIT: usize = 8 * 6144 / 8;

struct DecodedAudioPacket {
    samples: Vec<Vec<f32>>,
    failed: bool,
}

/// How far a track's nominal rate may sit from a standard rate and still be read as it. Timestamps
/// rounded to a coarse unit (whole milliseconds in a 90 kHz track, as phones and messaging apps
/// write them) move the average by less than this at every standard rate (#1700).
const STANDARD_RATE_TOLERANCE: f64 = 0.005;

/// A video track's nominal frame rate: the mean of the sample durations within a quarter frame of
/// the median duration, read as the closest standard rate when close enough.
///
/// A duration near the median is the same frame period as rounding has quantised it; one much
/// longer belongs to a variable-rate frame and stays out of the mean. The mean sees through a
/// millisecond rounder the median cannot: 30 fps over 90 kHz is 2970 and 3060 units in turn, whose
/// median reads 30.303 fps but whose mean is 30 fps (#1700).
fn nominal_rate(timescale: u32, samples: &[Sample]) -> Option<FrameRate> {
    let mut durs: Vec<u32> = samples.iter().map(|s| s.duration).filter(|&d| d > 0).collect();
    durs.sort_unstable();
    let &median = durs.get(durs.len() / 2)?;
    let near: Vec<f64> = durs.iter().filter(|&&d| d.abs_diff(median) * 4 <= median).map(|&d| f64::from(d)).collect();
    let mean = near.iter().sum::<f64>() / near.len().max(1) as f64;
    if mean <= 0.0 {
        return None;
    }
    Some(FrameRate::from_f64_within(f64::from(timescale) / mean, STANDARD_RATE_TOLERANCE).sane())
}

struct AudioState {
    decoder: Option<PacketDecoder>,
    /// Decoded packets by sample index.
    packets: HashMap<usize, Arc<DecodedAudioPacket>>,
    order: Vec<usize>,
    last_decoded: Option<usize>,
}

/// One playable audio track with its own decoder state and timing tables.
struct Mp4Audio {
    /// Index into `Mp4File::tracks`.
    track: usize,
    state: Mutex<AudioState>,
    /// Packet start positions in source sample frames (see [`audio_starts`]).
    starts: Vec<i64>,
    /// Presentation offset of the track in its timescale (edit list, or Opus pre-skip).
    offset: i64,
    /// Decoder pre-roll after a seek, in source sample frames (0: prime with one packet).
    preroll: i64,
}

pub struct Mp4Source {
    info: MediaInfo,
    bytes: crate::Src,
    file: Mp4File,
    vtrack: Option<usize>,
    video: GopCache,
    /// Look frames up by the start of their frame: the video track's timestamp unit is half a frame
    /// or longer and its samples sit on the nominal frame grid ([`crate::gop::snap_coarse_units`]).
    snap: bool,
    /// The playable audio tracks in file order: `info.audio_streams[k]` describes `audios[k]`.
    audios: Vec<Mp4Audio>,
}

pub fn sniff(b: &[u8]) -> bool {
    b.len() >= 12 && matches!(&b[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip")
}

/// Audio packet start positions in source sample frames at `rate`: the running total of the sample
/// durations, except that packets which all decode to the same length (AAC, MPEG audio, AC-3,
/// E-AC-3) run on from one another, resynchronising to the total only across gaps of more than half
/// a packet.
/// A remux from Matroska (OBS's) carries the millisecond rounding of its timestamps into the
/// durations: 1008, 1008, 1056… for 1024-sample AAC frames at 48 kHz (see
/// [`crate::audio::contiguous_starts`]).
fn audio_starts(file: &Mp4File, bytes: &crate::Src, ti: usize, rate: u32) -> Vec<i64> {
    use crate::audio::{FixedFrames, PacketTime, contiguous_starts, fixed_packet_samples};
    let Some(t) = file.tracks.get(ti) else { return Vec::new() };
    let ts = i128::from(t.timescale.max(1));
    let mut total = 0i128;
    let stamps: Vec<i64> = t
        .samples
        .iter()
        .map(|s| {
            let at = total * i128::from(rate) / ts;
            total += i128::from(s.duration);
            i64::try_from(at).unwrap_or(i64::MAX)
        })
        .collect();
    let first = || file.read_sample(bytes, ti, 0).unwrap_or_default();
    let frame = match t.entries.first().map(|e| &e.codec) {
        Some(CodecConfig::Aac(a)) => fixed_packet_samples(FixedFrames::Aac(&a.asc), &[], rate),
        Some(CodecConfig::Mp3) => fixed_packet_samples(FixedFrames::MpegAudio, &first(), rate),
        Some(CodecConfig::Ac3 { .. }) => fixed_packet_samples(FixedFrames::Ac3, &first(), rate),
        Some(CodecConfig::Eac3 { .. }) => fixed_packet_samples(FixedFrames::Eac3, &first(), rate),
        _ => None,
    };
    let Some(frame) = frame else { return stamps };
    // samples per track timescale unit, rounded up: how coarse the running total is
    let unit = i64::try_from((i128::from(rate) + ts - 1) / ts).unwrap_or(i64::MAX);
    let packets = stamps.iter().map(|&stamp| PacketTime { stamp, stamped: true, samples: Some(frame) });
    contiguous_starts(packets, (frame / 2).max(unit.saturating_mul(2).saturating_add(1)))
}

/// Return source-rate buffer indices overlapped by a failed packet's concealment span.
/// Packet positions are audio frames, but duration is raw track-timescale ticks
/// treated as a frame count here; the span is clipped to the request.
fn failed_audio_span(packet_start: i64, request_start: i64, duration: u32, request_len: usize) -> std::ops::Range<usize> {
    let begin = usize::try_from(packet_start.saturating_sub(request_start).max(0)).unwrap_or(usize::MAX).min(request_len);
    let end = if duration == 0 {
        begin
    } else {
        let last = packet_start.saturating_add(i64::from(duration) - 1).saturating_sub(request_start);
        usize::try_from((i128::from(last) + 1).max(0)).unwrap_or(usize::MAX).min(request_len)
    };
    begin..end
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

/// The clean aperture (`clap`, ISO/IEC 14496-12 / QuickTime) of a `w`×`h` decoded picture as a
/// pixel rectangle: its size, centred on the picture and moved by its offsets, rounded to whole
/// samples. `None` (the whole picture is shown) when it is the whole picture or makes no sense: a
/// zero denominator, an empty or larger-than-the-picture size, a rectangle outside the picture.
/// [`VideoFrame::cropped`] moves an odd corner onto the chroma grid.
fn clean_aperture(c: CleanAperture, (w, h): (u32, u32)) -> Option<Region> {
    let q = |(n, d): (u32, u32)| (d != 0).then(|| f64::from(n) / f64::from(d));
    let qs = |(n, d): (i32, u32)| (d != 0).then(|| f64::from(n) / f64::from(d));
    let (cw, ch, dx, dy) = (q(c.width)?.round(), q(c.height)?.round(), qs(c.horiz_offset)?, qs(c.vert_offset)?);
    let (fw, fh) = (f64::from(w), f64::from(h));
    if !(cw >= 1.0 && ch >= 1.0 && cw <= fw && ch <= fh) {
        return None;
    }
    let (x, y) = (((fw - cw) / 2.0 + dx).round(), ((fh - ch) / 2.0 + dy).round());
    // also false for NaN
    if !(x >= 0.0 && y >= 0.0 && x + cw <= fw && y + ch <= fh) {
        return None;
    }
    let r = Region { x: x as usize, y: y as usize, w: cw as usize, h: ch as usize };
    (!r.is_full(w as usize, h as usize)).then_some(r)
}

/// Colour of a video sample entry. The `colr` box (`nclx` / `nclc`) wins; what it leaves
/// unspecified, or all of it when there is none (ffmpeg writes MP4 without `colr` by default),
/// comes from the stream's own description: `vpcC` / `apvC`, or the SPS VUI in `avcC` / `hvcC`
/// and the sequence header in `av1C`. The rest defaults by frame size.
fn color_from(entry: &filmcraft_isobmff::SampleEntry, w: u32, h: u32) -> ColorInfo {
    use crate::stream_color::ColorCodes;
    use filmcraft_isobmff::ColorInfo as Colr;
    let colr = match entry.video.as_ref().and_then(|v| v.color.as_ref()) {
        Some(Colr::Nclx { primaries, transfer, matrix, full_range }) => {
            Some(ColorCodes::from_wide((*primaries).into(), (*transfer).into(), (*matrix).into(), Some(*full_range)))
        }
        // QuickTime `nclc` has no range flag
        Some(Colr::Nclc { primaries, transfer, matrix }) => Some(ColorCodes::from_wide((*primaries).into(), (*transfer).into(), (*matrix).into(), None)),
        // an ICC profile, or no `colr`: no code points
        _ => None,
    };
    let stream = match &entry.codec {
        CodecConfig::Vp9(v) => Some(ColorCodes {
            primaries: v.colour_primaries,
            transfer: v.transfer_characteristics,
            matrix: v.matrix_coefficients,
            full_range: Some(v.full_range),
        }),
        CodecConfig::Apv(a) if a.color_description_present => Some(ColorCodes {
            primaries: a.color_primaries,
            transfer: a.transfer_characteristics,
            matrix: a.matrix_coefficients,
            full_range: Some(a.full_range),
        }),
        CodecConfig::Apv(_) => None,
        codec => crate::stream_color::from_codec_config(codec),
    };
    let sources: Vec<ColorCodes> = [colr, stream].into_iter().flatten().collect();
    crate::stream_color::resolve(w, h, &sources)
}

/// First bytes of a Sony `rtmd` sample needed for the start-timecode block (`frames` is last).
const RTMD_HEADER_LEN: usize = 0x12;

/// Sony XAVC `rtmd` (real-time metadata) start timecode fields, read from the first sample: hours,
/// minutes, seconds, a drop-frame flag and frames. Sony stores them as raw decimal bytes in the
/// sample's fixed header. The layout is proprietary and undocumented, so it is reproduced here from
/// real footage; it is the same block ffmpeg reports as the `timecode` tag of an `rtmd` stream.
///
/// Fields outside SMPTE bounds — including the labels drop-frame counting skips — are rejected
/// (no fabricated start time from damaged metadata).
fn rtmd_fields(sample: &[u8], rate: FrameRate) -> Option<(i64, i64, i64, i64, bool)> {
    let f = sample.get(0x0d..0x12)?;
    let (h, m, s) = (f[0] as i64, f[1] as i64, f[2] as i64);
    let frames = f[4] as i64;
    let drop = f[3] != 0;
    let base = rate.timecode_base();
    if h > 23 || m > 59 || s > 59 || frames >= base {
        return None;
    }
    // Drop-frame counting skips the first `base / 15` frame labels of every minute except each
    // tenth minute, so those labels never occur in valid footage (SMPTE ST 12-1).
    if drop && rate.supports_drop_frame() && s == 0 && m % 10 != 0 && frames < base / 15 {
        return None;
    }
    Some((h, m, s, frames, drop))
}

/// Start timecode from a Sony `rtmd` track as a frame count at `rate` (issue #460), used when the
/// file has no `tmcd` track. `None` if there is no `rtmd` track or it has no usable first sample.
/// Only the fixed-size header is read (a damaged `stsz` size can't drive a huge allocation).
fn rtmd_start_timecode(file: &Mp4File, bytes: &crate::Src, rate: FrameRate) -> Option<i64> {
    let t = file.tracks.iter().find(|t| t.entries.first().is_some_and(|e| e.format.0 == *b"rtmd"))?;
    let s = t.samples.first()?;
    if s.size < RTMD_HEADER_LEN as u32 {
        return None;
    }
    let mut buf = [0u8; RTMD_HEADER_LEN];
    bytes.read_at(s.offset, &mut buf).ok()?;
    let (h, m, s, f, drop) = rtmd_fields(&buf, rate)?;
    Some(filmcraft_time::fields_to_frames(h, m, s, f, rate, drop))
}

impl Mp4Source {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    /// Open from a random-access reader: only the index is read now, samples on demand.
    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bytes = crate::Src(reader);
        let mut file = filmcraft_isobmff::open(&bytes).map_err(|e| CodecError::Container(e.to_string()))?;
        // A track needs samples, a sample description and a timescale to be playable (damaged
        // files can lack them: indexing `entries[0]` or dividing by the timescale used to panic).
        let playable = |t: &&filmcraft_isobmff::Track, kind| t.kind == kind && !t.samples.is_empty() && !t.entries.is_empty() && t.timescale > 0;
        let vtrack = file.tracks.iter().position(|t| playable(&t, TrackKind::Video));
        // every audio track is a stream (the track count comes from the file: capped)
        let atracks: Vec<usize> =
            file.tracks.iter().enumerate().filter(|(_, t)| playable(t, TrackKind::Audio)).map(|(i, _)| i).take(filmcraft_media::MAX_AUDIO_STREAMS).collect();
        if vtrack.is_none() && atracks.is_empty() {
            return Err(CodecError::Unsupported("no playable tracks".into()));
        }
        let mut color = ColorInfo::REC709;
        let mut explicit_color = None;
        // display rotation from the track matrix (portrait phone video is stored landscape)
        let rotation = vtrack.and_then(|i| file.tracks[i].display_rotation()).unwrap_or(0);
        // the clean aperture ('clap') cut from every frame, before the rotation
        let mut aperture = None;
        let video = vtrack.map(|i| {
            let t = &file.tracks[i];
            let entry = &t.entries[0];
            let vp = entry.video.clone().unwrap_or_default();
            let (w, h) = (if vp.width > 0 { vp.width as u32 } else { t.width }, if vp.height > 0 { vp.height as u32 } else { t.height });
            // H.264 and HEVC say their own output size (frame cropping / conformance window), which
            // is what the decoder returns; a sample entry can give the coded size instead (#288)
            let (w, h) = match crate::hw::NalStreamInfo::from_entry(entry) {
                Some(Ok(n)) if n.crop.2 > 0 && n.crop.3 > 0 => (n.crop.2, n.crop.3),
                _ => (w, h),
            };
            aperture = vp.clean_aperture.and_then(|c| clean_aperture(c, (w, h)));
            let (w, h) = aperture.map_or((w, h), |r| (r.w as u32, r.h as u32));
            // A rate that rounds to zero (one sample per huge duration) can't be divided by.
            let rate = nominal_rate(t.timescale, &t.samples).unwrap_or_default();
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
        // one entry per playable audio track (`playable` guarantees a sample description); a track
        // that somehow has none is dropped so `audio_streams` and `audios` stay index-aligned
        let (atracks, audio_streams): (Vec<usize>, Vec<AudioStreamInfo>) =
            atracks.into_iter().filter_map(|i| Some((i, audio_stream_info(file.tracks.get(i)?, &bytes)?))).unzip();
        let duration = vtrack.or(atracks.first().copied()).and_then(|i| file.tracks.get(i)).map(|t| presentation_duration(&file, t)).unwrap_or_default();
        let start_timecode = file.tracks.iter().find_map(|t| match t.codec() {
            Some(CodecConfig::Timecode(tc)) => tc.start_frame.map(|f| f as i64),
            _ => None,
        });
        // Sony XAVC files with no `tmcd` track carry the start timecode in an `rtmd` metadata
        // track; convert its fields to a frame count using the video frame rate.
        let start_timecode = start_timecode.or_else(|| rtmd_start_timecode(&file, &bytes, video.as_ref()?.frame_rate));
        let info = MediaInfo {
            name: name.to_string(),
            kind: if video.is_some() { MediaKind::Movie } else { MediaKind::AudioOnly },
            duration,
            video,
            audio_streams,
            container: if file.is_quicktime { "QuickTime".into() } else { "MPEG-4".into() },
            start_timecode,
            file_size: Some(bytes.0.len()),
        };
        let mut audios: Vec<Mp4Audio> = atracks
            .iter()
            .zip(&info.audio_streams)
            .filter_map(|(&i, ainfo)| {
                let t = file.tracks.get(i)?;
                let starts = audio_starts(&file, &bytes, i, ainfo.sample_rate);
                let ts = t.timescale.max(1) as i64;
                let (offset, preroll) = match t.entries.first().map(|e| &e.codec) {
                    Some(CodecConfig::Opus(o)) => {
                        let off = if t.edit_offset != 0 { t.edit_offset } else { -(o.pre_skip as i64) * ts / 48_000 };
                        // (Opus plays at 48 kHz: the pre-roll is in its sample frames)
                        (off, crate::audio::OPUS_PRE_ROLL as i64)
                    }
                    _ => (t.edit_offset, 0),
                };
                let state = Mutex::new(AudioState { decoder: None, packets: HashMap::new(), order: Vec::new(), last_decoded: None });
                Some(Mp4Audio { track: i, state, starts, offset, preroll })
            })
            .collect();
        snap_to_frame_grid(&mut file, vtrack);
        rebase_unedited_video(&mut file, vtrack, &mut audios);
        let snap = vtrack.zip(info.video.as_ref()).is_some_and(|(i, v)| {
            let t = &file.tracks[i];
            crate::gop::snap_coarse_units(t.samples.iter().map(|s| s.pts.saturating_add(t.edit_offset)), 1, i64::from(t.timescale), v.frame_rate)
        });
        Ok(Self { info, bytes, file, vtrack, video: GopCache::new(explicit_color).with_rotation(rotation).with_crop(aperture), snap, audios })
    }

    fn read(&self, track: usize, i: usize) -> crate::Result<Vec<u8>> {
        self.file.read_sample(&self.bytes, track, i).map_err(|e| CodecError::Container(e.to_string()))
    }

    fn video_at(&self, t: Tick) -> crate::Result<Arc<VideoFrame>> {
        let ti = self.vtrack.ok_or_else(|| CodecError::Unsupported("no video".into()))?;
        let ts = self.file.tracks[ti].timescale as i64;
        let rate = self.info.video.as_ref().map(|v| v.frame_rate).unwrap_or_default();
        let at = |t: Tick| crate::gop::lookup_time(t, 1, ts, rate, self.snap);
        let late = filmcraft_media::cancel::catch_up().map(|m| at(t - m));
        self.video.frame_late(&Mp4Video { src: self, track: ti }, at(t), late)
    }

    /// Decoded packet `i` of audio stream `a` (`ainfo` is its `AudioStreamInfo`; `st` its decoder state).
    fn audio_packet(&self, a: &Mp4Audio, ainfo: &AudioStreamInfo, st: &mut AudioState, i: usize) -> crate::Result<Arc<DecodedAudioPacket>> {
        if let Some(p) = st.packets.get(&i) {
            return Ok(p.clone());
        }
        let ti = a.track;
        let track = self.file.tracks.get(ti).ok_or_else(|| CodecError::Unsupported("no audio track".into()))?;
        let entry = track.entries.first().ok_or_else(|| CodecError::Container("audio track without a sample description".into()))?;
        track.samples.get(i).ok_or_else(|| CodecError::Container("audio packet out of range".into()))?;
        let pk_start = a.starts.get(i).copied().unwrap_or(0);
        let data = self.read(ti, i)?;
        let (samples, failed) = match &entry.codec {
            CodecConfig::Pcm(p) => (decode_pcm(&data, p), false),
            c => {
                if st.decoder.is_none() {
                    st.decoder = Some(PacketDecoder::for_isobmff(c, ainfo.sample_rate)?);
                }
                // Non-sequential access: reset and prime with the preceding packets (codec pre-roll:
                // one packet, or `OPUS_PRE_ROLL` worth for Opus).
                if st.last_decoded.is_none_or(|l| l + 1 != i) {
                    let d = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?;
                    d.reset();
                    let from = if a.preroll > 0 {
                        a.starts.partition_point(|&x| x <= pk_start.saturating_sub(a.preroll)).saturating_sub(1)
                    } else {
                        i.saturating_sub(1)
                    };
                    for j in from..i {
                        if let Ok(prev) = self.read(ti, j) {
                            let _ = d.decode(&prev, 0);
                        }
                    }
                }
                let result = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?.decode(&data, pk_start.max(0) as u64);
                st.last_decoded = Some(i);
                match result {
                    Ok(samples) => (samples, false),
                    Err(_) => (Vec::new(), true),
                }
            }
        };
        let p = Arc::new(DecodedAudioPacket { samples, failed });
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
        self.audio_stream(0, start, frames, sample_rate)
    }

    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let a = self.audios.get(stream).ok_or(MediaError::NoStream("audio"))?;
        let ti = a.track;
        let track = self.file.tracks.get(ti).ok_or(MediaError::NoStream("audio"))?;
        let ainfo = self.info.audio_streams.get(stream).ok_or(MediaError::NoStream("audio"))?;
        let src_rate = ainfo.sample_rate;
        let ch = ainfo.channels.max(1) as usize;
        let source_frames = filmcraft_media::resample_source_frames(frames, src_rate, sample_rate)?;
        let need = source_frames.checked_add(2).ok_or_else(|| MediaError::Decode("audio source window overflows".into()))?;
        filmcraft_media::validate_audio_buffer_shape(ch, need)?;
        filmcraft_media::validate_audio_buffer_shape(ch, frames)?;
        // Map the requested window to source samples (edit list offset applied: presentation = pts + edit_offset).
        let ratio = src_rate as f64 / sample_rate as f64;
        let p0 = (start as f64 * ratio).floor() as i64;
        // (in i128: a hostile edit list can make the offset large enough to overflow i64)
        let offset = i128::from(a.offset) * i128::from(src_rate) / i128::from(track.timescale.max(1));
        let s0 = i64::try_from(i128::from(p0) - offset).unwrap_or(if offset > 0 { i64::MIN } else { i64::MAX });
        let need_i64 = i64::try_from(need).unwrap_or(i64::MAX);
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need]; ch];
        let mut st = a.state.lock().unwrap_or_else(|e| e.into_inner());
        let first = a.starts.partition_point(|&x| x <= s0.max(0)).saturating_sub(1);
        let mut i = first;
        while let Some(sample) = track.samples.get(i) {
            let Some(&pk_start) = a.starts.get(i) else { break };
            if pk_start >= s0.saturating_add(need_i64) {
                break;
            }
            let pk = self.audio_packet(a, ainfo, &mut st, i)?;
            if pk.failed {
                let range = failed_audio_span(pk_start, s0, sample.duration, src.first().map_or(0, Vec::len));
                for dst in &mut src {
                    if let Some(span) = dst.get_mut(range.clone()) {
                        span.fill(0.0);
                    }
                }
                i += 1;
                continue;
            }
            for (c, dst) in src.iter_mut().enumerate() {
                let Some(chan) = pk.samples.get(c.min(pk.samples.len().saturating_sub(1))) else { continue };
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

/// Channel layout and sample rate metadata for AC-3 / E-AC-3 in ISO-BMFF. The sample-entry channel
/// count is required to be two, but the real layout is in `dac3` / `dec3` (ETSI TS 102 366 Annex F).
/// Without a usable descriptor, a primary syncframe header is a safe fallback.
fn ac3_stream_info(t: &filmcraft_isobmff::Track, bytes: &crate::Src) -> Option<(u32, u32)> {
    let entry = t.entries.first()?;
    if let Some((rate, channels)) = match &entry.codec {
        CodecConfig::Ac3 { dac3 } => dac3_layout(dac3),
        CodecConfig::Eac3 { dec3 } => dec3_layout(dec3),
        _ => None,
    } {
        return Some((rate, channels));
    }
    let data = t.samples.first().and_then(|s| filmcraft_media::reader::read_range(&*bytes.0, s.offset, s.size as usize).ok());
    let h = filmcraft_ac3::parse_header(data.as_deref()?).ok()?;
    // A dependent E-AC-3 substream doesn't describe the full program; report only if the first
    // packet is the primary independent substream.
    let is_primary = if let CodecConfig::Eac3 { .. } = &entry.codec { h.is_primary() } else { true };
    if !is_primary {
        return None;
    }
    let rate = h.sample_rate;
    let channels = u32::try_from(h.channels()).ok()?;
    (rate > 0 && channels > 0).then_some((rate, channels))
}

/// E-AC-3 `ec-3` entry: the program-layout fields are the 24 bits starting at byte 2 of the
/// descriptor, then `bsmod(5) | acmod(3) | lfeon(1)`. Matches FFmpeg's `dec3` demuxer.
fn dec3_layout(payload: &[u8]) -> Option<(u32, u32)> {
    if payload.len() < 5 {
        return None;
    }
    let bits = u32::from(payload[2]) << 16 | u32::from(payload[3]) << 8 | u32::from(payload[4]);
    let acmod = ((bits >> 9) & 0x7) as u8;
    let lfeon = bits & 0x100 != 0;
    let channels = dac3_channels(acmod, lfeon)?;
    Some((0, channels))
}

/// AC-3 `ac-3` entry: `dac3` begins with the syncframe bitstream information: sample rate code,
/// bit stream id, bit stream mode, channel mode and LFE flag. The descriptor has no sample rate,
/// so the caller keeps the sample-entry rate.
fn dac3_layout(payload: &[u8]) -> Option<(u32, u32)> {
    if payload.len() < 3 {
        return None;
    }
    // The descriptor begins with the bitstream information: fscod(2), bsid(5), bsmod(3),
    // acmod(3) and lfeon(1), continuing in the second payload byte.
    let bits = u32::from_be_bytes([0, 0, payload[1], payload[2]]);
    let acmod = ((bits >> 11) & 0x7) as u8;
    let lfeon = bits & 0x400 != 0;
    let channels = dac3_channels(acmod, lfeon)?;
    Some((0, channels))
}

/// Full-bandwidth channels per AC-3 channel mode (Table 5.8) plus the optional LFE channel.
fn dac3_channels(acmod: u8, lfeon: bool) -> Option<u32> {
    let n = [2u32, 1, 2, 3, 3, 4, 4, 5].get(usize::from(acmod & 7))?.checked_add(u32::from(lfeon))?;
    (n > 0).then_some(n)
}

/// What the file says about one audio track (`None`: the track has no sample description).
fn audio_stream_info(t: &filmcraft_isobmff::Track, bytes: &crate::Src) -> Option<AudioStreamInfo> {
    let entry = t.entries.first()?;
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
            // the configuration is an index (7 and 12 are both eight channels), not a count
            if a.channels() > 0 { a.channels() } else { ap.channels },
            None,
        ),
        CodecConfig::Pcm(p) => (p.sample_rate as u32, p.channels, Some(p.bits as u32)),
        CodecConfig::Opus(o) => (crate::audio::OPUS_RATE, (o.output_channels as u32).max(1), None),
        CodecConfig::Ac3 { .. } | CodecConfig::Eac3 { .. } => match ac3_stream_info(t, bytes) {
            // A syncframe fallback names the sample rate; a descriptor alone does not, so keep
            // the sample-entry rate, which the muxer must supply.
            Some((rate, ch)) if rate > 0 => (rate, ch, None),
            // A partial descriptor can still say the exact number of channels even without
            // enough bytes for the sample-rate field.
            Some((_, ch)) => (if ap.sample_rate > 0.0 { ap.sample_rate as u32 } else { t.timescale }, ch, None),
            // Without a descriptor, a normal primary-frame fallback gives both fields.
            None if ap.sample_rate > 0.0 => (ap.sample_rate as u32, ap.channels.max(1), None),
            None => (t.timescale, ap.channels.max(1), None),
        },
        _ => (if ap.sample_rate > 0.0 { ap.sample_rate as u32 } else { t.timescale }, ap.channels.max(1), None),
    };
    let codec = match &entry.codec {
        // these entries name no layer (ffmpeg stores MP2 as `mp4a` + 0x6B): the first frame does
        CodecConfig::Mp3 => {
            let head = t.samples.first().and_then(|s| filmcraft_media::reader::read_range(&*bytes.0, s.offset, (s.size as usize).min(4)).ok());
            match head.as_deref().and_then(crate::audio::mpeg_audio_layer) {
                Some(1 | 2) => "MPEG Audio".into(),
                _ => codec_label(&entry.codec),
            }
        }
        c => codec_label(c),
    };
    let (rate, ch) = filmcraft_media::bounded_audio_dimensions(rate, ch).ok()?;
    Some(AudioStreamInfo { sample_rate: rate, channels: ch, codec, bits_per_sample: bits })
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
        CodecConfig::Unknown { fourcc, .. } if fourcc.0 == *b"vp08" => "VP8".into(),
        CodecConfig::Unknown { fourcc, .. } if fourcc.0 == *b"raw " => "Uncompressed RGB".into(),
        CodecConfig::Unknown { fourcc, .. } if fourcc.0 == *b"2vuy" => "Uncompressed YUV 4:2:2".into(),
        CodecConfig::Unknown { fourcc, .. } if fourcc.0 == *b"v210" => "Uncompressed YUV 4:2:2 10-bit".into(),
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

/// `chroma_format_idc` and luma bit depth of an H.264 track: from its first SPS, which is what the
/// decoder decodes, else from the `avcC` High-profile extension (ISO/IEC 14496-15 §5.3.3). The
/// extension is not trusted over the SPS: muxers have been seen writing `chroma_format` 0 (4:0:0)
/// into it for 4:2:0 files, which labelled them "4:0:0" (#1959). The profile alone can't say (High
/// 4:2:2 is 8 or 10-bit, High 10 can be 8-bit).
fn avc_format(a: &filmcraft_isobmff::AvcConfig) -> Option<(u32, u32)> {
    let from_sps = a.sps.first().and_then(|n| n.get(1..)).and_then(|rbsp| filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(rbsp)).ok());
    if let Some(sps) = from_sps {
        return Some((sps.chroma_format_idc, sps.bit_depth_luma));
    }
    // reserved bits set: '111111' chroma_format, '11111' bit_depth_luma_minus8 (else not an extension)
    if !matches!(a.profile, 66 | 77 | 88)
        && let &[chroma, depth, ..] = a.ext.as_slice()
        && chroma & 0xFC == 0xFC
        && depth & 0xF8 == 0xF8
    {
        return Some(((chroma & 3) as u32, (depth & 7) as u32 + 8));
    }
    None
}

/// `chroma_format_idc` and luma bit depth of an HEVC track: from its first SPS, which is what the
/// decoder decodes, else the `hvcC` record's own fields (which a muxer may leave at 0, i.e. "4:0:0").
fn hevc_format(h: &filmcraft_isobmff::HevcConfig) -> (u32, u32) {
    let from_sps = h.sps().first().and_then(|n| n.get(2..)).and_then(|rbsp| filmcraft_hevc::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(rbsp)).ok());
    match from_sps {
        Some(sps) => (sps.chroma_format_idc, sps.bit_depth_luma),
        None => (h.chroma_format_idc as u32, h.bit_depth_luma as u32),
    }
}

fn pixfmt_label(c: &CodecConfig) -> String {
    match c {
        CodecConfig::Avc(a) => avc_format(a).map(|(chroma, bits)| yuv_label(chroma, bits)).unwrap_or_default(),
        CodecConfig::Hevc(h) => {
            let (chroma, bits) = hevc_format(h);
            yuv_label(chroma, bits)
        }
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

/// Put the video samples of a constant-rate file whose pictures sit just off its frame grid back
/// on the grid (#1282). Phone footage stamps frames up to a millisecond after where they belong (a
/// first frame 34.1 ms long at 30 fps), and the frame shown at a time is the last one that starts at
/// or before it: a sequence frame asked at the exact grid time got the picture before.
///
/// The grid starts at the first picture and steps by the nominal frame duration. A file is snapped
/// only when every picture is less than a quarter of a frame from its own grid point and no two land
/// on the same one: a frame that far off was placed there (variable frame rate), so a file with one
/// such picture keeps all its times. A picture at most one unit off is left where it is: that can't
/// be told from the timescale's rounding. Snapping keeps the samples' presentation order. Only the video samples move: the first picture, which the audio and
/// `rebase_unedited_video` go by, is the grid's origin, and the clip's duration comes from the
/// track header.
fn snap_to_frame_grid(file: &mut Mp4File, vtrack: Option<usize>) {
    let Some(t) = vtrack.and_then(|i| file.tracks.get_mut(i)) else { return };
    let Some(first) = t.samples.iter().map(|s| s.pts).min() else { return };
    // The nominal duration: the mean of the samples near the median, with the rate snapped to its
    // standard form when rounding moved it. Whole-millisecond timestamps turn 30 fps into 33/34 ms
    // durations; the median alone picks either side, while the recovered mean keeps the whole grid.
    let Some(rate) = nominal_rate(t.timescale, &t.samples) else { return };
    // one frame lasts `step_num / step_den` track units
    let (step_num, step_den) = (i128::from(t.timescale) * i128::from(rate.den), i128::from(rate.num));
    if step_num <= 0 || step_den <= 0 {
        return;
    }
    // a sample's grid index and its new time
    let snap = |pts: i64| -> Option<(i128, i64)> {
        let off = i128::from(pts) - i128::from(first);
        // the nearest grid index, then its grid point (both rounded half up)
        let n = (2 * off * step_den + step_num).div_euclid(2 * step_num);
        let at = i128::from(first) + (2 * n * step_num + step_den).div_euclid(2 * step_den);
        let off_grid = (i128::from(pts) - at).abs();
        if off_grid * 4 * step_den >= step_num {
            return None;
        }
        Some((n, if off_grid <= 1 { pts } else { i64::try_from(at).ok()? }))
    };
    let Some(snapped) = t.samples.iter().map(|s| snap(s.pts)).collect::<Option<Vec<_>>>() else { return };
    // Two pictures by one grid point: not a constant rate. Checked on the index, not the new time:
    // one picture kept a unit after the point and another moved onto it would be distinct times in
    // the wrong order.
    let mut points: Vec<i128> = snapped.iter().map(|&(n, _)| n).collect();
    points.sort_unstable();
    points.dedup();
    if points.len() != snapped.len() {
        return;
    }
    for (s, (_, p)) in t.samples.iter_mut().zip(snapped) {
        s.pts = p;
    }
}

/// Start the file at its first picture when its video track has no edit list (#714). Without an
/// edit list the media timeline is the presentation, so a B-frame stream's first picture sits at its
/// composition delay (two frames on an x264 MP4 written without `elst`, and on fragmented MP4s):
/// media time zero repeated it and the clip, which lasts the media duration, lost its last pictures.
/// Every track moves back by the earliest start among them, video and audio alike, so the streams
/// stay in sync and a delay between them keeps its meaning; a track with an edit list is never
/// rebased on its own.
fn rebase_unedited_video(file: &mut Mp4File, vtrack: Option<usize>, audios: &mut [Mp4Audio]) {
    let Some(vi) = vtrack else { return };
    let Some(t) = file.tracks.get(vi).filter(|t| t.edits.is_empty() && t.timescale > 0) else { return };
    let Some(first) = t.samples.iter().map(|s| s.pts).min().map(|p| p.saturating_add(t.edit_offset)).filter(|&p| p > 0) else { return };
    let vts = i64::from(t.timescale);
    let video_start = Tick::from_rational(first, 1, vts);
    let audio_ts = |a: &Mp4Audio| file.tracks.get(a.track).map_or(1, |t| i64::from(t.timescale.max(1)));
    // an audio track starts at its offset (a negative one only hides priming before zero)
    let shift = audios.iter().fold(video_start, |m, a| m.min(Tick::from_rational(a.offset.max(0), 1, audio_ts(a))));
    if shift <= Tick::ZERO {
        return;
    }
    for a in audios.iter_mut() {
        let d = shift.to_rational_round(1, audio_ts(a));
        a.offset = a.offset.saturating_sub(d);
    }
    let v = if shift == video_start { first } else { shift.to_rational_round(1, vts) };
    if let Some(t) = file.tracks.get_mut(vi) {
        t.edit_offset = t.edit_offset.saturating_sub(v);
    }
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
mod tests;

#[cfg(test)]
mod multi_audio_tests {
    use filmcraft_isobmff::{Brand, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    use filmcraft_media::MediaSource;

    /// Two mono PCM tracks with different rates and samples. Stream indices follow file order;
    /// decoder caches, sample timing and resampling must not cross streams.
    #[test]
    fn every_audio_track_is_inspected_and_decoded_independently() {
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
        for (rate, value) in [(48_000u32, 8192i16), (24_000, -16384)] {
            let entry = SampleEntry::pcm(PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 1, sample_rate: rate as f64 });
            let track = mux.add_track(TrackConfig::new(entry, rate)).unwrap();
            let data: Vec<u8> = (0..rate / 10).flat_map(|_| value.to_le_bytes()).collect();
            mux.write_sample(track, WriteSample { data: &data, duration: rate / 10, composition_offset: 0, is_sync: true }).unwrap();
        }
        let bytes = mux.finish().unwrap().into_inner();
        let source = super::Mp4Source::open("two.mov", bytes.into()).unwrap();
        assert_eq!(source.info().audio_streams.iter().map(|a| a.sample_rate).collect::<Vec<_>>(), vec![48_000, 24_000]);
        let primary = source.audio(0, 100, 48_000).unwrap();
        let secondary = source.audio_stream(1, 0, 100, 48_000).unwrap();
        assert!((primary.channels[0][50] - 0.25).abs() < 0.001);
        assert!((secondary.channels[0][50] + 0.5).abs() < 0.001);
        assert!(source.audio_stream(2, 0, 100, 48_000).is_err());
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
        assert_eq!(src.info().audio().unwrap().sample_rate, 44_100);
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

#[cfg(test)]
mod corrupt_audio_tests {
    use filmcraft_isobmff::{Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
    use filmcraft_media::MediaSource;

    #[test]
    fn failed_packet_span_matches_per_sample_silence_at_extreme_timestamps() {
        let min = i64::MIN;
        let max = i64::MAX;
        for start in [min, min + 1, min + 2, -10, -1, 0, 1, 10, max - 2, max - 1, max] {
            for request in [min, min + 1, min + 2, -10, -1, 0, 1, 10, max - 2, max - 1, max] {
                for duration in [0, 1, 2, 5, 128] {
                    let old: Vec<_> = (0..duration)
                        .map(|k| start.saturating_add(i64::from(k)).saturating_sub(request))
                        .filter(|&position| (0..8).contains(&position))
                        .map(|position| position as usize)
                        .collect();
                    let range = super::failed_audio_span(start, request, duration, 8);
                    let actual: Vec<_> = (0..8).filter(|position| range.contains(position)).collect();
                    let expected: Vec<_> = old.into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                    assert_eq!(actual, expected, "start={start}, request={request}, duration={duration}");
                }
            }
        }
        assert_eq!(super::failed_audio_span(0, 0, u32::MAX, 128), 0..128);
    }

    #[test]
    fn failed_aac_packet_does_not_materialize_its_hostile_duration() {
        // AAC-LC, 48 kHz stereo. The empty access unit cannot decode.
        let asc = vec![0x11, 0x90];
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
        let track = mux.add_track(TrackConfig::new(SampleEntry::aac(asc, 2, 48_000), 48_000)).unwrap();
        mux.write_sample(track, WriteSample { data: &[], duration: 1_000_000, composition_offset: 0, is_sync: true }).unwrap();
        let source = super::Mp4Source::open("damaged.m4a", mux.finish().unwrap().into_inner().into()).unwrap();
        let audio = &source.audios[0];
        let info = &source.info.audio_streams[0];
        let mut state = audio.state.lock().unwrap();
        let packet = source.audio_packet(audio, info, &mut state, 0).unwrap();
        assert!(packet.failed);
        assert!(packet.samples.is_empty(), "failed packet must not allocate from its untrusted duration");
        drop(state);

        let window = source.audio(0, 128, 48_000).unwrap();
        assert_eq!(window.channels.len(), 2);
        assert!(window.channels.iter().all(|ch| ch.len() == 128 && ch.iter().all(|&v| v == 0.0)));
    }

    #[test]
    fn failed_aac_packet_silences_overlap_independent_of_request_boundary() {
        let rate = 48_000u32;
        let tone: Vec<f32> = (0..8192).map(|i| 0.3 * (i as f32 * 0.13).sin()).collect();
        let mut encoder = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(rate, 1, 64_000)).unwrap();
        let mut units = encoder.encode(&[&tone]);
        units.extend(encoder.flush());
        assert!(units.len() >= 2);
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
        let track = mux.add_track(TrackConfig::new(SampleEntry::aac(vec![0x11, 0x88], 1, rate), rate)).unwrap();
        mux.write_sample(track, WriteSample { data: &units[0], duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
        mux.write_sample(track, WriteSample { data: &units[1], duration: 400, composition_offset: 0, is_sync: true }).unwrap();
        mux.write_sample(track, WriteSample { data: &[], duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
        let source = super::Mp4Source::open("overlap.m4a", mux.finish().unwrap().into_inner().into()).unwrap();
        // The failed packet starts at 1024 + 400 = 1424, after 124 frames of the crossing read.
        let crossing = source.audio(1300, 256, rate).unwrap();
        let isolated = source.audio(1424, 132, rate).unwrap();
        let prior_packet_len = {
            let audio = &source.audios[0];
            let info = &source.info.audio_streams[0];
            let mut state = audio.state.lock().unwrap();
            source.audio_packet(audio, info, &mut state, 1).unwrap().samples.first().map_or(0, Vec::len)
        };
        assert!(prior_packet_len > 400, "valid packet must overlap the failed packet");
        assert!(crossing.channels[0][..124].iter().any(|v| v.abs() > 0.001));
        assert!(isolated.channels[0].iter().all(|&v| v == 0.0));
        assert_eq!(&crossing.channels[0][124..], &isolated.channels[0]);
    }
}
