//! OS media integration (layer L5): hardware video decoding and encoding through the operating
//! system's codecs.
//!
//! [`register`] puts the platform's hardware decoder factory in front of FilmCraft's own decoders
//! (`filmcraft_codecs::register_video_decoder`). Today that is VideoToolbox on macOS for H.264
//! (`avcC`) and HEVC (`hvcC`) streams, 8- and 10-bit, 4:2:0 and 4:2:2; on other systems
//! registration does nothing and reports [`Availability::Unavailable`]. It also registers a
//! hardware H.264 encoder factory (`filmcraft_export::register_encoder`) that only acts when an
//! export asks for it (`ExportSettings::hardware_encoding` = `Auto`), see [`hardware_encode`].
//!
//! Hardware decoding never makes a file undecodable:
//!
//! - the factory declines (falls through to the software decoder) when Settings ▸ Playback ▸
//!   Hardware decoding is Off (`filmcraft_codecs::hw::set_hardware_decoding`), when the stream's
//!   format is one the hardware path does not take, or when the OS cannot create a hardware
//!   session for it (profile, size, no hardware decoder);
//! - a decoder that fails mid-stream switches to the software decoder transparently
//!   ([`HybridDecoder`]) and logs it.
//!
//! Pictures are the software decoder's: same planes (bit-exact on the parity fixtures), colour,
//! pixel aspect, pts and presentation order, so the two are interchangeable.
//!
//! This is the one crate allowed to use `unsafe` (OS FFI), and only in its FFI modules
//! (docs/adr/0001-platform-ffi.md).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// Used by the Windows decoder only; compiled everywhere so their tests run on every system.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod annexb;
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod biplanar;
#[cfg(target_os = "macos")]
pub mod hardware_encode;
pub mod hybrid;
#[cfg(target_os = "windows")]
pub mod media_foundation;
#[cfg(target_os = "windows")]
pub mod nvenc;
#[cfg(target_os = "linux")]
pub mod vaapi;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod videotoolbox;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod videotoolbox_encode;

pub use hybrid::HybridDecoder;

/// What [`register`] made available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// A hardware decoder factory was registered (its name).
    Available(&'static str),
    /// Nothing to register on this system (why).
    Unavailable(&'static str),
}

/// Register the platform's hardware video decoders (call once at startup; repeated calls are
/// harmless). Streams they do not take, and every stream while hardware decoding is Off, keep
/// using FilmCraft's own decoders.
pub fn register() -> Availability {
    #[cfg(target_os = "macos")]
    {
        filmcraft_codecs::register_video_decoder(videotoolbox_factory);
        filmcraft_export::register_encoder(hardware_encode::videotoolbox_encoder_factory);
        filmcraft_export::register_format_probe(filmcraft_export::Format::Hevc, hardware_encode::hevc_available);
        filmcraft_codecs::hw::set_hw_backend("VideoToolbox");
        Availability::Available("VideoToolbox")
    }
    #[cfg(target_os = "windows")]
    {
        static ENCODERS: std::sync::Once = std::sync::Once::new();
        // Export ▸ Hardware encoding (NVENC H.264): in front of the software encoder, taking an
        // export only when asked for and when NVENC can do it
        ENCODERS.call_once(|| filmcraft_export::register_encoder(nvenc::export::factory));
        filmcraft_codecs::register_video_decoder(media_foundation_factory);
        filmcraft_codecs::hw::set_hw_backend("Media Foundation");
        Availability::Available("Media Foundation")
    }
    #[cfg(target_os = "linux")]
    {
        // only when this system has a VA-API driver that decodes H.264 (libva is loaded lazily)
        if vaapi::device::Display::open_for(vaapi::ffi::VAProfileH264High, vaapi::ffi::VA_RT_FORMAT_YUV420).is_err() {
            return Availability::Unavailable("no VA-API hardware decoder (libva or a driver is missing)");
        }
        filmcraft_codecs::register_video_decoder(vaapi_factory);
        // Export ▸ Hardware encoding (H.264): in front of the software encoder, taking an export
        // only when asked for and when the GPU can do it
        static ENCODERS: std::sync::Once = std::sync::Once::new();
        ENCODERS.call_once(|| {
            filmcraft_export::register_encoder(vaapi::h264enc::factory);
            // H.265 has no software encoder: the format exists where the GPU encodes it
            filmcraft_export::register_encoder(vaapi::hevcenc::factory);
            filmcraft_export::register_format_probe(filmcraft_export::Format::Hevc, vaapi::hevcenc::available);
        });
        filmcraft_codecs::hw::set_hw_backend("VA-API");
        Availability::Available("VA-API")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Availability::Unavailable("no hardware video decoder for this system yet")
    }
}

/// Whether [`register`] has put a hardware decoder factory in front of our decoders.
pub fn registered() -> bool {
    #[cfg(target_os = "macos")]
    {
        filmcraft_codecs::video_decoder_registered(videotoolbox_factory)
    }
    #[cfg(target_os = "windows")]
    {
        filmcraft_codecs::video_decoder_registered(media_foundation_factory)
    }
    #[cfg(target_os = "linux")]
    {
        filmcraft_codecs::video_decoder_registered(vaapi_factory)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// Whether this system's hardware decoder takes the stream of `entry` (whatever the Hardware
/// decoding setting says): diagnostics and tests.
pub fn hardware_decoder_for(entry: &filmcraft_isobmff::SampleEntry) -> bool {
    #[cfg(target_os = "macos")]
    {
        filmcraft_codecs::hw::NalStreamInfo::from_entry(entry).and_then(|r| r.ok()).is_some_and(|info| videotoolbox::VtDecoder::new(info).is_ok())
    }
    #[cfg(target_os = "windows")]
    {
        media_foundation::stream_info(entry).is_some_and(|info| media_foundation::MfDecoder::new(info).is_ok())
    }
    #[cfg(target_os = "linux")]
    {
        vaapi_decoder(entry).is_some_and(|r| r.is_ok())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = entry;
        false
    }
}

/// The VA-API decoder for `entry` (H.264 8-bit and HEVC 8/10-bit, 4:2:0 progressive), `None`
/// when it does not take the stream; `Some(Err)` when it does but the hardware could not start.
#[cfg(target_os = "linux")]
fn vaapi_decoder(entry: &filmcraft_isobmff::SampleEntry) -> Option<std::result::Result<Box<dyn filmcraft_codecs::VideoDecoder>, String>> {
    use filmcraft_codecs::hw::{FrameCodec, FrameStreamInfo, NalStreamInfo};
    type Boxed = Box<dyn filmcraft_codecs::VideoDecoder>;
    if let Some(info) = NalStreamInfo::from_entry(entry) {
        let info = info.ok()?;
        if info.interlaced || info.chroma_format_idc != 1 || info.bit_depth_luma != info.bit_depth_chroma {
            return None;
        }
        let hw: std::result::Result<Boxed, String> = match &entry.codec {
            filmcraft_isobmff::CodecConfig::Avc(avc) if info.bit_depth_luma == 8 => {
                vaapi::h264::VaH264Decoder::new(&avc.to_bytes()).map(|d| Box::new(d) as Boxed)
            }
            filmcraft_isobmff::CodecConfig::Hevc(hevc) if matches!(info.bit_depth_luma, 8 | 10) => {
                vaapi::hevc::VaHevcDecoder::new(&hevc.to_bytes(), info.bit_depth_luma).map(|d| Box::new(d) as Boxed)
            }
            _ => return None,
        };
        return Some(hw.map(|d| Box::new(HybridDecoder::new(d, entry.clone(), info)) as Boxed));
    }
    let info = FrameStreamInfo::from_entry(entry)?.ok()?;
    if info.mono || info.subsampling != (1, 1) {
        return None;
    }
    let hw: std::result::Result<Boxed, String> = match info.codec {
        FrameCodec::Vp9 if matches!((info.profile, info.bit_depth), (0, 8) | (2, 10)) => {
            vaapi::vp9::VaVp9Decoder::new(info.clone()).map(|d| Box::new(d) as Boxed)
        }
        FrameCodec::Av1 if info.profile == 0 && matches!(info.bit_depth, 8 | 10) => vaapi::av1::VaAv1Decoder::new(info.clone()).map(|d| Box::new(d) as Boxed),
        _ => return None,
    };
    Some(hw.map(|d| Box::new(HybridDecoder::new(d, entry.clone(), info)) as Boxed))
}

/// The VA-API factory: a [`HybridDecoder`] around [`vaapi::h264::VaH264Decoder`] for streams the
/// GPU can decode, `None` otherwise (and while Hardware decoding is Off).
#[cfg(target_os = "linux")]
pub fn vaapi_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    match vaapi_decoder(entry)? {
        Ok(d) => Some(Ok(d)),
        Err(why) => {
            log::info!("hardware decoding declined for {} video: {why}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}

/// The VideoToolbox factory: a [`HybridDecoder`] around [`videotoolbox::VtDecoder`] for `avcC` /
/// `hvcC` streams VideoToolbox can decode in hardware, `None` otherwise.
#[cfg(target_os = "macos")]
pub fn videotoolbox_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry)?.ok()?;
    match videotoolbox::VtDecoder::new(info.clone()) {
        Ok(vt) => Some(Ok(Box::new(HybridDecoder::new(Box::new(vt), entry.clone(), info)))),
        Err(why) => {
            log::info!("hardware decoding declined for {} video: {why}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}

/// The Media Foundation factory: a [`HybridDecoder`] around [`media_foundation::MfDecoder`] for
/// H.264 / HEVC / VP9 / AV1 streams a Direct3D-aware decoder MFT can decode with DXVA on this
/// system's GPU, `None` otherwise.
#[cfg(target_os = "windows")]
pub fn media_foundation_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = media_foundation::stream_info(entry)?;
    match media_foundation::MfDecoder::new(info.clone()) {
        Ok(mf) => Some(Ok(Box::new(HybridDecoder::new(Box::new(mf), entry.clone(), info)))),
        Err(why) => {
            log::info!("hardware decoding declined for {} video: {why}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}
