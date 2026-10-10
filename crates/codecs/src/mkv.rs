//! Matroska/WebM media source (`filmcraft-matroska` demux, GOP-aware video via [`crate::gop`]).
//!
//! Video codecs are mapped onto ISO-BMFF sample entries so the same decoder factories serve both
//! containers (H.264, HEVC, VP9, AV1, ProRes, MJPEG). Audio: AAC and Opus
//! via our decoders, PCM directly, MP3/FLAC/Vorbis via the bootstrap decoders.
//!
//! Opus (`A_OPUS`): `CodecPrivate` is the `OpusHead`; output is always 48 kHz. The demuxer
//! subtracts `CodecDelay` from timestamps, so the pre-skip samples land before zero and are never
//! read; random access decodes `SeekPreRoll` (at least [`crate::audio::OPUS_PRE_ROLL`]) of preceding
//! packets before the target. Packet starts are accumulated from TOC durations (see [`opus_starts`]).
//!
//! AAC, MPEG audio and AC-3 packets each decode to a fixed number of samples, which millisecond
//! timestamps can't express: their packets run on from one another (see [`audio_starts`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filmcraft_color::{ColorInfo, Matrix, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_isobmff::{AvcConfig, CodecConfig, FourCc, HevcConfig, PcmConfig, SampleEntry, VpcConfig};
use filmcraft_matroska::{Codec, MkvFile, TrackKind};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_time::{FrameRate, Tick};

use crate::audio::{PacketDecoder, decode_pcm};
use crate::gop::{GopCache, VideoSamples};
use crate::stream_color::ColorCodes;
use crate::video::VideoDecoder;
use crate::{CodecError, make_video_decoder};

pub fn sniff(b: &[u8]) -> bool {
    b.len() >= 4 && b[..4] == [0x1A, 0x45, 0xDF, 0xA3]
}

struct AudioState {
    decoder: Option<PacketDecoder>,
    packets: HashMap<usize, Arc<Vec<Vec<f32>>>>,
    order: Vec<usize>,
    last_decoded: Option<usize>,
}

/// One playable audio track with its own decoder state and timing tables.
struct MkvAudio {
    /// Index into `MkvFile::tracks`.
    track: usize,
    state: Mutex<AudioState>,
    /// Packet start positions in source sample frames.
    starts: Vec<i64>,
    /// Decoder pre-roll after a seek, in source sample frames (0: prime with one packet).
    preroll: i64,
}

pub struct MkvSource {
    info: MediaInfo,
    bytes: crate::Src,
    file: MkvFile,
    vtrack: Option<usize>,
    /// Video codec as an ISO-BMFF sample entry (for the decoder factories).
    ventry: Option<SampleEntry>,
    video: GopCache,
    /// The alpha layer of a VP9 track with `AlphaMode` 1 (WebM transparency, #402).
    alpha: Option<Arc<crate::webm_alpha::AlphaLayer>>,
    /// The playable audio tracks in file order: `info.audio_streams[k]` describes `audios[k]`.
    audios: Vec<MkvAudio>,
}

/// Audio packet start positions in source sample frames.
///
/// Block timestamps are quantised to `TimestampScale` (usually 1 ms), so they are only accurate to a
/// tick: a 1024-sample AAC frame at 48 kHz lasts 21.33 ms. Packets that all decode to the same
/// length (AAC, MPEG audio, AC-3, E-AC-3) therefore run on from one another, resynchronising to the
/// timestamp only across gaps of more than half a packet (and more than two ticks); see
/// [`crate::audio::contiguous_starts`]. Opus packets: [`opus_starts`]. Other codecs start at their
/// timestamps.
fn audio_starts(file: &MkvFile, bytes: &crate::Src, ti: usize, rate: i64) -> Vec<i64> {
    use crate::audio::{FixedFrames, PacketTime, contiguous_starts, fixed_packet_samples};
    let Some(t) = file.tracks.get(ti) else { return Vec::new() };
    if let Some(h) = opus_head(&t.codec) {
        return opus_starts(file, bytes, ti, &h, rate);
    }
    let (n, d) = tb(t);
    let at = |pts: i64| (pts as i128 * n as i128 * rate as i128 / d as i128) as i64;
    let first = || file.read_sample(bytes, ti, 0).unwrap_or_default();
    let out_rate = u32::try_from(rate).unwrap_or(0);
    let frame = match &t.codec {
        Codec::Aac { asc } => fixed_packet_samples(FixedFrames::Aac(asc), &[], out_rate),
        Codec::Mp3 | Codec::Mp2 => fixed_packet_samples(FixedFrames::MpegAudio, &first(), out_rate),
        Codec::Ac3 => fixed_packet_samples(FixedFrames::Ac3, &first(), out_rate),
        Codec::Eac3 => fixed_packet_samples(FixedFrames::Eac3, &first(), out_rate),
        _ => None,
    };
    let Some(frame) = frame else {
        return t.samples.iter().map(|s| at(s.pts)).collect();
    };
    let packets = t.samples.iter().map(|s| PacketTime { stamp: at(s.pts), stamped: own_stamp(s), samples: Some(frame) });
    contiguous_starts(packets, (frame / 2).max(stamp_tolerance(t, rate)))
}

/// Sample-exact Opus packet start positions (48 kHz frames, pre-skip removed).
///
/// The demuxer also subtracts a rounded `CodecDelay` from the timestamps. Starts are accumulated
/// from each packet's TOC duration, resynchronising to the timestamp only across real gaps (more
/// than two ticks off). Pre-skip is the exact `CodecDelay` (or the header's pre-skip when it is
/// absent).
fn opus_starts(file: &MkvFile, bytes: &crate::Src, ti: usize, head: &filmcraft_opus::OpusHead, rate: i64) -> Vec<i64> {
    let Some(t) = file.tracks.get(ti) else { return Vec::new() };
    let (n, d) = tb(t);
    let delay_ns = t.codec_delay_ns as i128;
    // The demuxer's rounding of CodecDelay to ticks, undone here.
    let delay_ticks = codec_delay_ticks(t);
    let skip = if t.codec_delay_ns > 0 { (delay_ns * rate as i128 / 1_000_000_000) as i64 } else { head.pre_skip as i64 };
    let at = |pts: i64| ((pts.saturating_add(delay_ticks) as i128 * n as i128 * rate as i128 / d as i128) as i64).saturating_sub(skip);
    let packets = t.samples.iter().enumerate().map(|(i, s)| crate::audio::PacketTime {
        stamp: at(s.pts),
        stamped: own_stamp(s),
        samples: file.read_sample(bytes, ti, i).ok().and_then(|p| crate::audio::opus_packet_samples(&p)).map(|k| k as i64),
    });
    crate::audio::contiguous_starts(packets, stamp_tolerance(t, rate))
}

/// `CodecDelay` in timestamp ticks, rounded as the demuxer rounds it (half away from zero) before
/// subtracting it from every timestamp of the track.
fn codec_delay_ticks(t: &filmcraft_matroska::Track) -> i64 {
    let (n, d) = tb(t);
    let scale_ns = (n as i128 * 1_000_000_000 / d as i128).max(1);
    i64::try_from((t.codec_delay_ns as i128 + scale_ns / 2) / scale_ns).unwrap_or(i64::MAX)
}

/// The track timestamp that media time zero stands for (#715): the earliest start among the
/// playable tracks. Block timestamps count from the start of the Segment, and a remux that keeps
/// its source's timestamps (`ffmpeg -copyts`, a cut from a recording) starts its first picture
/// seconds in; taken as media time, that repeated the opening picture until then. An audio track
/// starts where its first packet does before `CodecDelay` was taken off (the delay only hides
/// priming), and never before zero.
fn media_origin(file: &MkvFile, vtrack: Option<usize>, atracks: &[usize]) -> i64 {
    let start = |i: usize| {
        let t = file.tracks.get(i)?;
        let first = t.samples.iter().map(|s| s.pts).min()?;
        Some(if t.kind == TrackKind::Audio { first.saturating_add(codec_delay_ticks(t)).max(0) } else { first })
    };
    vtrack.into_iter().chain(atracks.iter().copied()).filter_map(start).min().unwrap_or(0).max(0)
}

/// Move every track back by `origin` timestamp ticks, so the streams keep their relative timing
/// (a delay between picture and sound survives).
fn rebase(file: &mut MkvFile, origin: i64) {
    if origin <= 0 {
        return;
    }
    for s in file.tracks.iter_mut().flat_map(|t| t.samples.iter_mut()) {
        s.pts = s.pts.saturating_sub(origin);
    }
}

/// End of the last sample of track `i`, in its timebase.
fn track_end(file: &MkvFile, i: usize) -> Option<(i64, &filmcraft_matroska::Track)> {
    let t = file.tracks.get(i)?;
    Some((t.samples.iter().map(|s| s.pts.saturating_add(i64::try_from(s.duration).unwrap_or(i64::MAX))).max().unwrap_or(0), t))
}

/// Two timestamp ticks in sample frames at `rate`, plus one: how far a packet's Matroska timestamp
/// can be from its exact start.
fn stamp_tolerance(t: &filmcraft_matroska::Track, rate: i64) -> i64 {
    let (n, d) = tb(t);
    i64::try_from(2 * n as i128 * rate as i128 / d as i128).unwrap_or(i64::MAX).saturating_add(1)
}

/// Whether a sample's timestamp is its own: frames after the first in a laced block without a
/// duration repeat the block's timestamp.
fn own_stamp(s: &filmcraft_matroska::Sample) -> bool {
    s.lace == 0 || s.duration > 0
}

/// The parsed `OpusHead` of an `A_OPUS` track.
fn opus_head(c: &Codec) -> Option<filmcraft_opus::OpusHead> {
    match c {
        Codec::Opus { head } => filmcraft_opus::OpusHead::parse(head).ok(),
        _ => None,
    }
}

/// Seconds per track timestamp unit as a rational.
fn tb(t: &filmcraft_matroska::Track) -> (i64, i64) {
    (t.timebase.0.max(1) as i64, t.timebase.1.max(1) as i64)
}

fn to_tick(t: &filmcraft_matroska::Track, pts: i64) -> Tick {
    let (n, d) = tb(t);
    Tick::from_rational(pts, n, d)
}

/// A time as the track timestamp to look a frame up by: rounded to the nearest tick, as muxers
/// round frame times to `TimestampScale` (usually 1 ms). Flooring skipped every frame whose
/// timestamp was rounded up (every third frame at 30 fps).
fn from_tick(t: &filmcraft_matroska::Track, time: Tick) -> i64 {
    let (n, d) = tb(t);
    time.to_rational_round(n, d)
}

/// VP9 configuration from the Matroska `CodecPrivate` feature list (ID / length / value triples:
/// 1 profile, 2 level, 3 bit depth, 4 chroma subsampling) and the track's `Colour`.
fn vp9_config(private: &[u8], v: Option<&filmcraft_matroska::VideoInfo>) -> VpcConfig {
    let mut c = VpcConfig { bit_depth: 8, colour_primaries: 2, transfer_characteristics: 2, matrix_coefficients: 2, ..Default::default() };
    let mut p = 0;
    while p + 2 <= private.len() {
        let (id, len) = (private[p], private[p + 1] as usize);
        let Some(val) = private.get(p + 2..p + 2 + len) else { break };
        let x = val.first().copied().unwrap_or(0);
        match id {
            1 => c.profile = x,
            2 => c.level = x,
            3 => c.bit_depth = x,
            4 => c.chroma_subsampling = x,
            _ => {}
        }
        p += 2 + len;
    }
    if let Some(col) = v.and_then(|v| v.colour.as_ref()) {
        if let Some(t) = col.transfer_characteristics {
            c.transfer_characteristics = t as u8;
        }
        if let Some(pr) = col.primaries {
            c.colour_primaries = pr as u8;
        }
        if let Some(m) = col.matrix_coefficients {
            c.matrix_coefficients = m as u8;
        }
        c.full_range = col.full_range();
    }
    c
}

fn sample_entry(c: &Codec, private: &[u8], v: Option<&filmcraft_matroska::VideoInfo>, w: u16, h: u16) -> Option<SampleEntry> {
    Some(match c {
        Codec::Vp9 { private } => SampleEntry::video(FourCc(*b"vp09"), CodecConfig::Vp9(vp9_config(private, v)), w, h),
        Codec::Av1 { av1c } => SampleEntry::video(FourCc(*b"av01"), CodecConfig::Av1(filmcraft_isobmff::Av1Config::parse(av1c).unwrap_or_default()), w, h),
        Codec::Apv { apvc } => SampleEntry::apv(filmcraft_isobmff::ApvConfig::parse(apvc).unwrap_or_default(), w, h),
        Codec::Avc { avcc } => SampleEntry::avc(AvcConfig::parse(avcc).ok()?, w, h),
        Codec::Hevc { hvcc } => SampleEntry::hevc(HevcConfig::parse(hvcc).ok()?, w, h),
        Codec::ProRes { fourcc } => SampleEntry::prores(FourCc(fourcc.unwrap_or(*b"apcn")), w, h),
        Codec::Mjpeg => SampleEntry::jpeg(w, h),
        // MPEG-1/2 video: the factory finds `mp2v` (CodecPrivate holds the sequence header)
        Codec::Other(id) if id == "V_MPEG2" || id == "V_MPEG1" => {
            SampleEntry::video(FourCc(*b"mp2v"), CodecConfig::Unknown { fourcc: FourCc(*b"mp2v"), raw: private.to_vec() }, w, h)
        }
        _ => return None,
    })
}

/// HDR static metadata from the Matroska `Colour` element.
fn hdr_metadata(c: &filmcraft_matroska::Colour) -> Option<filmcraft_color::HdrMetadata> {
    if c.mastering.is_none() && c.max_cll.is_none() && c.max_fall.is_none() {
        return None;
    }
    let nz = |v: f64| (v > 0.0).then_some(v as f32);
    Some(filmcraft_color::HdrMetadata {
        mastering_max_nits: c.mastering.as_ref().and_then(|m| nz(m.luminance_max)),
        mastering_min_nits: c.mastering.as_ref().map(|m| m.luminance_min as f32),
        max_cll: c.max_cll.and_then(|v| nz(v as f64)),
        max_fall: c.max_fall.and_then(|v| nz(v as f64)),
    })
}

/// Colour of a video track: the `Colour` element wins; what it leaves unspecified, or all of it
/// when there is none, comes from the stream's own description (the SPS VUI in an `avcC` /
/// `hvcC`, the sequence header in an `av1C`); the rest defaults by frame size. The flag says
/// whether `Colour` signals anything that overrides the decoder.
fn color_of(v: &filmcraft_matroska::VideoInfo, w: u32, h: u32, stream: Option<ColorCodes>) -> (ColorInfo, bool) {
    // H.273 code points; `Range`: 1 broadcast (limited), 2 full, 0 / 3 not signalled here
    let code = |c: Option<u64>| c.unwrap_or(2);
    let container = v.colour.as_ref().map(|col| {
        let range = match col.range {
            Some(1) => Some(false),
            Some(2) => Some(true),
            _ => None,
        };
        ColorCodes::from_wide(code(col.primaries), code(col.transfer_characteristics), code(col.matrix_coefficients), range)
    });
    let explicit = container.is_some_and(|c| Matrix::from_code(c.matrix).is_some() || Transfer::from_code(c.transfer).is_some() || c.full_range == Some(true));
    let sources: Vec<ColorCodes> = [container, stream].into_iter().flatten().collect();
    (crate::stream_color::resolve(w, h, &sources), explicit)
}

fn codec_label(c: &Codec) -> String {
    match c {
        Codec::Avc { .. } => "H.264".into(),
        Codec::Hevc { .. } => "HEVC".into(),
        Codec::Vp8 => "VP8".into(),
        Codec::Vp9 { .. } => "VP9".into(),
        Codec::Av1 { .. } => "AV1".into(),
        Codec::Apv { apvc } => match filmcraft_isobmff::ApvConfig::parse(apvc).ok().and_then(|a| filmcraft_apv::Profile::from_idc(a.profile_idc)) {
            Some(p) => p.name().into(),
            None => "APV".into(),
        },
        Codec::ProRes { .. } => "Apple ProRes".into(),
        Codec::Mjpeg => "Motion JPEG".into(),
        Codec::Aac { .. } => "AAC".into(),
        Codec::Opus { .. } => "Opus".into(),
        Codec::Vorbis { .. } => "Vorbis".into(),
        Codec::Flac { .. } => "FLAC".into(),
        Codec::Pcm { bits, float, .. } => format!("PCM {bits}-bit{}", if *float { " float" } else { "" }),
        Codec::Mp3 => "MP3".into(),
        Codec::Mp2 => "MPEG Audio".into(),
        Codec::Other(id) if id == "V_MPEG2" => "MPEG-2 Video".into(),
        Codec::Other(id) if id == "V_MPEG1" => "MPEG-1 Video".into(),
        other => other.name().to_string(),
    }
}

impl MkvSource {
    pub fn open(name: &str, bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open_reader(name, Arc::new(filmcraft_media::reader::MemReader(bytes)))
    }

    /// Open from a random-access reader: only the index is read now, samples on demand.
    pub fn open_reader(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let bytes = crate::Src(reader);
        let mut file = filmcraft_matroska::open(&bytes).map_err(|e| CodecError::Container(e.to_string()))?;
        let vtrack = file.tracks.iter().position(|t| t.kind == TrackKind::Video && !t.samples.is_empty());
        // every audio track is a stream (the track count comes from the file: capped)
        let atracks: Vec<usize> = file
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Audio && !t.samples.is_empty())
            .map(|(i, _)| i)
            .take(filmcraft_media::MAX_AUDIO_STREAMS)
            .collect();
        if vtrack.is_none() && atracks.is_empty() {
            return Err(CodecError::Unsupported("no playable tracks".into()));
        }
        let main = vtrack.or(atracks.first().copied());
        // The Segment's end (`Info/Duration` or the last sample), in Segment time.
        let segment_end = match file.duration_ns() {
            Some(ns) => Tick::from_rational(i64::try_from(ns).unwrap_or(i64::MAX), 1, 1_000_000_000),
            None => main.and_then(|i| track_end(&file, i)).map(|(end, t)| to_tick(t, end)).unwrap_or_default(),
        };
        // Media time zero is the first picture (or sound): the streams start at their common origin.
        let origin = media_origin(&file, vtrack, &atracks);
        rebase(&mut file, origin);
        // `Info/Duration` is the Segment's end: the media lasts from the origin to there (never less
        // than its main track's samples).
        let duration = match main.and_then(|i| track_end(&file, i)) {
            Some((end, t)) if origin > 0 => {
                let (n, d) = tb(t);
                (segment_end - Tick::from_rational(origin, n, d)).max(to_tick(t, end))
            }
            _ => segment_end,
        };
        let mut explicit_color = None;
        let mut ventry = None;
        // display rotation from the track's Projection (portrait phone video is stored landscape)
        let rotation = vtrack.and_then(|i| file.tracks[i].video.as_ref()).and_then(|v| v.display_rotation()).unwrap_or(0);
        let video = vtrack.map(|i| {
            let t = &file.tracks[i];
            let v = t.video.clone().unwrap_or_default();
            let (w, h) = (v.pixel_width, v.pixel_height);
            let rate = match t.default_duration_ns {
                Some(ns) if ns > 0 => FrameRate::from_f64(1e9 / ns as f64),
                _ => {
                    let mut d: Vec<i64> = t.samples.windows(2).take(240).map(|p| (p[1].pts - p[0].pts).abs()).filter(|d| *d > 0).collect();
                    d.sort_unstable();
                    let (n, dd) = tb(t);
                    let step = d.get(d.len() / 2).copied().unwrap_or(1).max(1) as f64 * n as f64 / dd as f64;
                    FrameRate::from_f64(1.0 / step)
                }
            };
            ventry = sample_entry(&t.codec, &t.codec_private, t.video.as_ref(), w as u16, h as u16);
            let (color, explicit) = color_of(&v, w, h, ventry.as_ref().and_then(|e| crate::stream_color::from_codec_config(&e.codec)));
            if explicit {
                explicit_color = Some(color);
            }
            let secs = duration.seconds();
            let bitrate = (secs > 0.0).then(|| (t.samples.iter().map(|s| s.size as u64).sum::<u64>() as f64 * 8.0 / secs) as u64);
            let ((w, h), par) = if rotation % 2 == 1 { ((h, w), (v.pixel_aspect().1, v.pixel_aspect().0)) } else { ((w, h), v.pixel_aspect()) };
            VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: rate,
                par,
                codec: codec_label(&t.codec),
                pixel_format: String::new(),
                color,
                has_alpha: v.alpha_mode != 0,
                bitrate,
                hdr: v.colour.as_ref().and_then(hdr_metadata),
            }
        });
        let audio_streams: Vec<AudioStreamInfo> = atracks
            .iter()
            .filter_map(|&i| file.tracks.get(i))
            .map(|t| {
                let a = t.audio.clone().unwrap_or_default();
                let mut rate = a.output_sampling_frequency.unwrap_or(a.sampling_frequency).round().max(1.0) as u32;
                let mut channels = (a.channels as u32).max(1);
                if let Some(h) = opus_head(&t.codec) {
                    rate = crate::audio::OPUS_RATE;
                    channels = h.channels as u32;
                }
                AudioStreamInfo { sample_rate: rate, channels, codec: codec_label(&t.codec), bits_per_sample: a.bit_depth.map(|b| b as u32) }
            })
            .collect();
        let info = MediaInfo {
            name: name.to_string(),
            kind: if video.is_some() { MediaKind::Movie } else { MediaKind::AudioOnly },
            duration,
            video,
            audio_streams,
            container: if file.is_webm() { "WebM".into() } else { "Matroska".into() },
            start_timecode: None,
            file_size: Some(bytes.0.len()),
        };
        // `info.audio_streams[k]` was made from `atracks[k]`, so the two stay index-aligned
        let audios: Vec<MkvAudio> = atracks
            .iter()
            .zip(&info.audio_streams)
            .filter_map(|(&i, ainfo)| {
                let t = file.tracks.get(i)?;
                let rate = ainfo.sample_rate as i64;
                let starts = audio_starts(&file, &bytes, i, rate);
                let preroll = match t.codec {
                    Codec::Opus { .. } => {
                        let container = (t.seek_pre_roll_ns as u128 * rate as u128).div_ceil(1_000_000_000) as i64;
                        container.max(crate::audio::OPUS_PRE_ROLL as i64 * rate / 48_000)
                    }
                    _ => 0,
                };
                let state = Mutex::new(AudioState { decoder: None, packets: HashMap::new(), order: Vec::new(), last_decoded: None });
                Some(MkvAudio { track: i, state, starts, preroll })
            })
            .collect();
        // WebM transparency: VP9 alpha in each block's BlockAdditional (ID 1)
        let alpha = vtrack.and_then(|i| file.tracks.get(i)).filter(|t| {
            matches!(t.codec, Codec::Vp9 { .. }) && t.video.as_ref().is_some_and(|v| v.alpha_mode != 0) && t.samples.iter().any(|s| s.addition.is_some())
        });
        let alpha = alpha.map(|t| Arc::new(crate::webm_alpha::AlphaLayer::new(bytes.clone(), t.samples.iter().map(|s| (s.pts, s.addition)))));
        Ok(Self { info, bytes, file, vtrack, ventry, video: GopCache::new(explicit_color).with_rotation(rotation), alpha, audios })
    }

    fn read(&self, track: usize, i: usize) -> crate::Result<Vec<u8>> {
        self.file.read_sample(&self.bytes, track, i).map_err(|e| CodecError::Container(e.to_string()))
    }

    fn audio_decoder(&self, c: &Codec, rate: u32) -> crate::Result<PacketDecoder> {
        use symphonia::core::codecs::{CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CODEC_TYPE_VORBIS};
        match c {
            Codec::Aac { asc } => PacketDecoder::aac(asc, rate),
            Codec::Opus { head } => PacketDecoder::opus(filmcraft_opus::OpusHead::parse(head).map_err(|e| CodecError::Unsupported(format!("Opus: {e}")))?),
            Codec::Mp3 => PacketDecoder::new(CODEC_TYPE_MP3, rate, None),
            Codec::Mp2 => PacketDecoder::mpeg_audio(2, rate),
            Codec::Ac3 | Codec::Eac3 => PacketDecoder::ac3(),
            // symphonia wants the STREAMINFO block body: skip `fLaC` + the 4-byte block header.
            Codec::Flac { private } => PacketDecoder::new(CODEC_TYPE_FLAC, rate, private.get(8..42).map(<[u8]>::to_vec)),
            Codec::Vorbis { headers } if headers.len() == 3 => {
                let mut extra = headers[0].clone();
                extra.extend_from_slice(&headers[2]);
                PacketDecoder::new(CODEC_TYPE_VORBIS, rate, Some(extra))
            }
            other => Err(CodecError::Unsupported(format!("{} audio", codec_label(other)))),
        }
    }

    /// Decoded packet `i` of audio stream `a` (`ainfo` is its `AudioStreamInfo`; `st` its decoder state).
    fn audio_packet(&self, a: &MkvAudio, ainfo: &AudioStreamInfo, st: &mut AudioState, i: usize) -> crate::Result<Arc<Vec<Vec<f32>>>> {
        if let Some(p) = st.packets.get(&i) {
            return Ok(p.clone());
        }
        let ti = a.track;
        let track = self.file.tracks.get(ti).ok_or_else(|| CodecError::Unsupported("no audio track".into()))?;
        let pk_start = a.starts.get(i).copied().ok_or_else(|| CodecError::Container("audio packet out of range".into()))?;
        let data = self.read(ti, i)?;
        let decoded = match &track.codec {
            Codec::Pcm { float, big_endian, bits } => {
                let cfg = PcmConfig {
                    bits: *bits,
                    float: *float,
                    big_endian: *big_endian,
                    signed: *bits > 8,
                    channels: ainfo.channels,
                    sample_rate: ainfo.sample_rate as f64,
                };
                decode_pcm(&data, &cfg)
            }
            c => {
                if st.decoder.is_none() {
                    st.decoder = Some(self.audio_decoder(c, ainfo.sample_rate)?);
                }
                // Non-sequential access: reset and prime with the preceding packets (codec pre-roll:
                // one packet, or `SeekPreRoll` worth for Opus).
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
                let r = st.decoder.as_mut().ok_or_else(|| CodecError::Decode("no audio decoder".into()))?.decode(&data, pk_start.max(0) as u64);
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

/// The Matroska video track as a [`VideoSamples`] table.
struct MkvVideo<'a> {
    src: &'a MkvSource,
    track: usize,
}

impl VideoSamples for MkvVideo<'_> {
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
        self.src.file.tracks[self.track].sample_at_pts(t)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        self.src.read(self.track, i)
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        let Some(e) = &self.src.ventry else {
            return Err(CodecError::Unsupported(format!("no decoder for {} video", codec_label(&self.src.file.tracks[self.track].codec))));
        };
        let color = make_video_decoder(e)?;
        let Some(layer) = &self.src.alpha else { return Ok(color) };
        // the alpha layer always goes through our own decoder (it is luma only)
        match crate::software_video_decoder(e) {
            Ok(alpha) => Ok(Box::new(crate::webm_alpha::AlphaDecoder::new(color, alpha, layer.clone()))),
            Err(_) => Ok(color),
        }
    }
}

impl MediaSource for MkvSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let ti = self.vtrack.ok_or(MediaError::NoStream("video"))?;
        let t = req.time.max(Tick::ZERO);
        let target = from_tick(&self.file.tracks[ti], t);
        let late = filmcraft_media::cancel::catch_up().map(|m| from_tick(&self.file.tracks[ti], t - m));
        Ok(self.video.frame_late(&MkvVideo { src: self, track: ti }, target, late)?)
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.audio_stream(0, start, frames, sample_rate)
    }

    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        let a = self.audios.get(stream).ok_or(MediaError::NoStream("audio"))?;
        let n_packets = self.file.tracks.get(a.track).ok_or(MediaError::NoStream("audio"))?.samples.len();
        let ainfo = self.info.audio_streams.get(stream).ok_or(MediaError::NoStream("audio"))?;
        let src_rate = ainfo.sample_rate;
        let ch = ainfo.channels.max(1) as usize;
        let ratio = src_rate as f64 / sample_rate as f64;
        let s0 = (start as f64 * ratio).floor() as i64;
        let need = (frames as f64 * ratio).ceil() as i64 + 2;
        let mut src: Vec<Vec<f32>> = vec![vec![0.0; need.max(0) as usize]; ch];
        let mut st = a.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut i = a.starts.partition_point(|&x| x <= s0).saturating_sub(1);
        while i < n_packets {
            let Some(&pk_start) = a.starts.get(i) else { break };
            if pk_start >= s0 + need {
                break;
            }
            let pk = self.audio_packet(a, ainfo, &mut st, i)?;
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

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(&bytes) {
        return None;
    }
    Some(MkvSource::open(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

/// [`filmcraft_media::ReaderOpener`] for Matroska/WebM.
pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !sniff(head) {
        return None;
    }
    Some(MkvSource::open_reader(name, reader.clone()).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

#[cfg(test)]
mod multi_audio_tests {
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    use filmcraft_media::MediaSource;

    #[test]
    fn every_audio_track_is_inspected_and_decoded_independently() {
        let tracks: Vec<TrackSpec> = [(48_000.0, "first"), (24_000.0, "second")]
            .into_iter()
            .map(|(rate, name)| {
                let mut t = TrackSpec::new(TrackKind::Audio, "A_PCM/INT/LIT");
                t.audio = Some((rate, 1, Some(16)));
                t.name = Some(name.into());
                t
            })
            .collect();
        let mut writer = MkvWriter::new(std::io::Cursor::new(Vec::new()), tracks, MuxOptions::default()).unwrap();
        for (stream, value) in [(0, 8192i16), (1, -16384)] {
            let data: Vec<u8> = (0..100).flat_map(|_| value.to_le_bytes()).collect();
            writer.write_frame(stream, 0, true, &data, Some(4_000_000)).unwrap();
        }
        let source = super::MkvSource::open("two.mkv", writer.finish().unwrap().into_inner().into()).unwrap();
        assert_eq!(source.info().audio_streams.iter().map(|a| a.sample_rate).collect::<Vec<_>>(), vec![48_000, 24_000]);
        let primary = source.audio(0, 50, 48_000).unwrap();
        let secondary = source.audio_stream(1, 0, 50, 48_000).unwrap();
        assert!((primary.channels[0][20] - 0.25).abs() < 0.001);
        assert!((secondary.channels[0][20] + 0.5).abs() < 0.001);
        assert!(source.audio_stream(2, 0, 50, 48_000).is_err());
    }
}

#[cfg(test)]
mod origin_tests {
    use filmcraft_frame::PixelData;
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    use filmcraft_media::{FrameRequest, MediaSource};
    use filmcraft_time::Tick;

    /// A flat grey Motion-JPEG picture: its level says which frame it is.
    fn picture(k: u8) -> Vec<u8> {
        let img = image::GrayImage::from_pixel(16, 16, image::Luma([20 + 20 * k]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95).encode_image(&img).unwrap();
        out
    }

    /// #715: a remux that keeps its source's timestamps (first picture at 1.48 s, sound 0.2 s after
    /// it) starts at its first picture, lasts as long as its pictures, and keeps the sound's delay.
    /// It repeated picture 0 until 1.48 s, and the clip ran to the Segment's absolute end.
    #[test]
    fn timestamp_preserving_remux_starts_at_the_first_picture() {
        let mut v = TrackSpec::new(TrackKind::Video, "V_MJPEG");
        v.video_size = Some((16, 16));
        v.default_duration_ns = Some(40_000_000);
        let mut a = TrackSpec::new(TrackKind::Audio, "A_PCM/INT/LIT");
        a.audio = Some((48_000.0, 1, Some(16)));
        let mut w = MkvWriter::new(std::io::Cursor::new(Vec::new()), vec![v, a], MuxOptions::default()).unwrap();
        for k in 0..10u8 {
            let pts = 1_480_000_000 + i64::from(k) * 40_000_000;
            w.write_frame(0, pts, true, &picture(k), None).unwrap();
            if k == 5 {
                let pcm: Vec<u8> = (0..960).flat_map(|_| 16_384i16.to_le_bytes()).collect();
                w.write_frame(1, 1_680_000_000, true, &pcm, Some(20_000_000)).unwrap();
            }
        }
        let src = super::MkvSource::open("shifted.mkv", w.finish().unwrap().into_inner().into()).unwrap();
        assert_eq!(src.info().duration, Tick::from_rational(10, 1, 25), "ten pictures at 25 fps");
        let level = |k: i64| {
            let f = src.video_frame(FrameRequest::full(Tick::from_rational(k, 1, 25))).unwrap();
            let PixelData::Rgba8(px) = &f.data else { panic!("MJPEG decodes to RGBA8") };
            px[0]
        };
        let seen: Vec<u8> = (0..10).map(level).collect();
        assert!(seen.windows(2).all(|p| p[0] < p[1]), "one picture per frame, in order: {seen:?}");
        let sound = src.audio(0, 48_000 * 2 / 5, 48_000).unwrap();
        assert!(sound.channels[0][9_590].abs() < 0.001, "silent before the delay");
        assert!((sound.channels[0][9_610] - 0.5).abs() < 0.001, "sound 0.2 s after the first picture");
    }
}
