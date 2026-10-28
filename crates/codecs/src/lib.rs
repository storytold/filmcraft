//! The container + codec hub.
//!
//! - [`VideoDecoder`]: the trait every video codec implements (our own H.264/HEVC/VP9/ProRes/…, MJPEG, and
//!   OS hardware decoders registered by the platform layer). Factories are tried in registration
//!   order, so a hardware decoder can take precedence over the pure-Rust one.
//! - [`Mp4Source`]: a [`MediaSource`](filmcraft_media::MediaSource) over MP4/MOV using
//!   `filmcraft-isobmff`: GOP-aware random access (seek to the preceding sync sample and decode
//!   forward, caching every decoded frame of the GOP), sequential fast path for playback, and
//!   packet-cached audio decoding.
//! - [`MkvSource`], [`MxfSource`] (OP1a / OP-Atom: AVC, MPEG-2 incl. D-10 / XDCAM, VC-3, ProRes,
//!   PCM) and [`OggSource`] (Ogg Opus with granule-position seeking, Ogg Vorbis): the same
//!   GOP-aware video access and packet-cached audio.
//! - [`MpegSource`]: MPEG-2 transport streams (`.ts`, `.m2ts`, `.mts`), program streams (`.mpg`,
//!   `.vob`, `.mod`) and MPEG-1/2 video elementary streams: MPEG-1/2, H.264 and HEVC video; MPEG
//!   audio, AAC (ADTS / LATM), AC-3 and LPCM.
//! - [`AviSource`]: AVI (OpenDML included): Motion JPEG, H.264, HEVC and uncompressed video; PCM,
//!   MP3 / MP2 and AC-3 audio.
//! - [`AudioFileSource`]: standalone compressed audio files (MP3, FLAC, AIFF, …).
//! - [`Y4mSource`]: uncompressed YUV4MPEG2 (`.y4m`) video, frames read on demand.
//! - [`openers`]: the openers to register with the engine's media pool.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod apv;
pub mod audio;
pub mod avi;
pub mod capabilities;
pub mod gop;
pub mod hw;
mod hw_frame;
pub mod mkv;
pub mod mp4;
pub mod mpeg;
pub mod mxf;
pub mod ogg;
mod stream_color;
pub mod video;
mod webm_alpha;
pub mod y4m;

use std::sync::{Arc, RwLock};

pub use apv::ApvSource;
pub use audio::AudioFileSource;
pub use avi::AviSource;
pub use gop::{FRAME_BUDGET, GopStats, cached_bytes, gop_stats, live_decoders};
pub use mkv::MkvSource;
pub use mp4::Mp4Source;
pub use mpeg::MpegSource;
pub use mxf::MxfSource;
pub use ogg::OggSource;
pub use video::{DecodedFrame, VideoDecoder, VideoDecoderFactory};
pub use y4m::Y4mSource;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("unsupported codec: {0}")]
    Unsupported(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("container: {0}")]
    Container(String),
    /// The frame is no longer wanted (`filmcraft_media::cancel`).
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, CodecError>;

impl From<CodecError> for filmcraft_media::MediaError {
    fn from(e: CodecError) -> Self {
        match e {
            CodecError::Unsupported(s) => filmcraft_media::MediaError::Unsupported(s),
            CodecError::Cancelled => filmcraft_media::MediaError::Cancelled,
            other => filmcraft_media::MediaError::Decode(other.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinVideoDecoder {
    H264,
    Hevc,
    Vp8,
    Vp9,
    Av1,
    Apv,
    Prores,
    Dnx,
    Mjpeg,
    Mpeg2,
    Raw,
}

impl BuiltinVideoDecoder {
    fn factory(self) -> VideoDecoderFactory {
        match self {
            Self::H264 => video::h264_factory,
            Self::Hevc => video::hevc_factory,
            Self::Vp8 => video::vp8_factory,
            Self::Vp9 => video::vp9_factory,
            Self::Av1 => video::av1_factory,
            Self::Apv => video::apv_factory,
            Self::Prores => video::prores_factory,
            Self::Dnx => video::dnx_factory,
            Self::Mjpeg => video::mjpeg_factory,
            Self::Mpeg2 => video::mpeg2_factory,
            Self::Raw => video::raw_factory,
        }
    }
}

/// Our own decoders, with stable identities and the existing dispatch order.
const BUILTIN_DECODERS: [BuiltinVideoDecoder; 11] = [
    BuiltinVideoDecoder::H264,
    BuiltinVideoDecoder::Hevc,
    BuiltinVideoDecoder::Vp8,
    BuiltinVideoDecoder::Vp9,
    BuiltinVideoDecoder::Av1,
    BuiltinVideoDecoder::Apv,
    BuiltinVideoDecoder::Prores,
    BuiltinVideoDecoder::Dnx,
    BuiltinVideoDecoder::Mjpeg,
    BuiltinVideoDecoder::Mpeg2,
    BuiltinVideoDecoder::Raw,
];

pub(crate) fn builtin_video_decoder_registered(decoder: BuiltinVideoDecoder) -> bool {
    BUILTIN_DECODERS.contains(&decoder)
}

fn factories() -> &'static RwLock<Vec<VideoDecoderFactory>> {
    static F: std::sync::OnceLock<RwLock<Vec<VideoDecoderFactory>>> = std::sync::OnceLock::new();
    F.get_or_init(|| RwLock::new(BUILTIN_DECODERS.iter().map(|d| d.factory()).collect()))
}

/// Register a video decoder factory (tried before previously registered ones).
pub fn register_video_decoder(f: VideoDecoderFactory) {
    let mut g = factories().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// Whether `f` is among the registered video decoder factories (startup diagnostics, tests).
pub fn video_decoder_registered(f: VideoDecoderFactory) -> bool {
    factories().read().unwrap_or_else(|e| e.into_inner()).iter().any(|x| std::ptr::fn_addr_eq(*x, f))
}

/// Create a decoder for a sample entry.
pub fn make_video_decoder(entry: &filmcraft_isobmff::SampleEntry) -> Result<Box<dyn VideoDecoder>> {
    let g = factories().read().unwrap_or_else(|e| e.into_inner());
    for f in g.iter() {
        if let Some(r) = f(entry) {
            return r;
        }
    }
    Err(CodecError::Unsupported(format!("no decoder for {} video", entry.codec.name())))
}

/// Create one of our own (software) decoders for a sample entry, skipping registered factories:
/// what a hardware decoder falls back to when it fails mid-stream.
pub fn software_video_decoder(entry: &filmcraft_isobmff::SampleEntry) -> Result<Box<dyn VideoDecoder>> {
    for decoder in BUILTIN_DECODERS {
        if let Some(result) = (decoder.factory())(entry) {
            return result;
        }
    }
    Err(CodecError::Unsupported(format!("no decoder for {} video", entry.codec.name())))
}

/// Openers for the engine's media pool (MP4/MOV, Matroska/WebM, MXF, Ogg Opus/Vorbis, MPEG TS/PS and
/// MPEG-1/2 video elementary streams, APV raw bitstreams, YUV4MPEG2, standalone audio).
pub fn openers() -> Vec<filmcraft_media::Opener> {
    vec![mp4::opener, mkv::opener, mxf::opener, ogg::opener, mpeg::opener, apv::opener, avi::opener, y4m::opener, audio::opener]
}

fn reader_registry() -> &'static RwLock<Vec<filmcraft_media::ReaderOpener>> {
    static R: std::sync::OnceLock<RwLock<Vec<filmcraft_media::ReaderOpener>>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        RwLock::new(vec![
            mp4::reader_opener,
            mkv::reader_opener,
            mxf::reader_opener,
            ogg::reader_opener,
            mpeg::reader_opener,
            apv::reader_opener,
            y4m::reader_opener,
            avi::reader_opener,
        ])
    })
}

/// Openers that read containers through a [`filmcraft_media::ByteReader`] (index now, samples on
/// demand) instead of the whole file, in the order they are tried.
pub fn reader_openers() -> Vec<filmcraft_media::ReaderOpener> {
    reader_registry().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Register a reader opener tried before the built-in ones (platform media sources such as the
/// web app's WebCodecs-decoded MP4).
pub fn register_reader_opener(f: filmcraft_media::ReaderOpener) {
    let mut g = reader_registry().write().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|x| std::ptr::fn_addr_eq(*x, f)) {
        g.insert(0, f);
    }
}

/// A media reader as the demuxers' byte source.
#[derive(Clone)]
pub(crate) struct Src(pub filmcraft_media::SharedReader);

impl filmcraft_isobmff::ByteSource for Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

impl filmcraft_avi::ByteSource for Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

impl filmcraft_matroska::ByteSource for Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

/// Convenience: an `Arc` media source from bytes (tries MP4/MOV then audio files).
pub fn open_bytes(name: &str, bytes: Arc<[u8]>) -> std::result::Result<filmcraft_media::SharedSource, filmcraft_media::MediaError> {
    filmcraft_media::open_bytes(name, bytes, &openers())
}

#[cfg(test)]
mod audio_timing_tests;
#[cfg(test)]
mod avi_tests;
#[cfg(test)]
mod rounded_pts_tests;
#[cfg(test)]
mod stream_color_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod builtin_registration_tests {
    use super::*;

    #[test]
    fn builtin_identities_keep_existing_order_and_v1_membership() {
        assert_eq!(
            BUILTIN_DECODERS,
            [
                BuiltinVideoDecoder::H264,
                BuiltinVideoDecoder::Hevc,
                BuiltinVideoDecoder::Vp8,
                BuiltinVideoDecoder::Vp9,
                BuiltinVideoDecoder::Av1,
                BuiltinVideoDecoder::Apv,
                BuiltinVideoDecoder::Prores,
                BuiltinVideoDecoder::Dnx,
                BuiltinVideoDecoder::Mjpeg,
                BuiltinVideoDecoder::Mpeg2,
                BuiltinVideoDecoder::Raw,
            ]
        );
        for decoder in [BuiltinVideoDecoder::H264, BuiltinVideoDecoder::Hevc, BuiltinVideoDecoder::Av1] {
            assert!(builtin_video_decoder_registered(decoder));
        }
        assert!(!BUILTIN_DECODERS[1..].contains(&BuiltinVideoDecoder::H264));
        assert_eq!(BUILTIN_DECODERS.len(), 11);
        // Each V1 identity recognizes its actual codec configuration; unrelated
        // factories decline it. Recognition does not claim stream conformance.
        use filmcraft_isobmff::{Av1Config, AvcConfig, CodecConfig, FourCc, HevcConfig, SampleEntry};
        let cases = [
            (BuiltinVideoDecoder::H264, SampleEntry::avc(AvcConfig { length_size: 4, ..Default::default() }, 16, 16)),
            (
                BuiltinVideoDecoder::Hevc,
                SampleEntry::hevc(
                    HevcConfig { general_profile_idc: 1, chroma_format_idc: 1, bit_depth_luma: 8, bit_depth_chroma: 8, length_size: 4, ..Default::default() },
                    16,
                    16,
                ),
            ),
            (
                BuiltinVideoDecoder::Av1,
                SampleEntry::video(
                    FourCc(*b"av01"),
                    CodecConfig::Av1(Av1Config { chroma_subsampling_x: true, chroma_subsampling_y: true, ..Default::default() }),
                    16,
                    16,
                ),
            ),
        ];
        for (expected, entry) in cases {
            for decoder in BUILTIN_DECODERS {
                assert_eq!((decoder.factory())(&entry).is_some(), decoder == expected);
            }
        }
    }
}
