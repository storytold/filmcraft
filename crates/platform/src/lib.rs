//! OS media integration (layer L5): hardware video decoding through the operating system's codecs.
//!
//! [`register`] puts the platform's hardware decoder factory in front of FilmCraft's own decoders
//! (`filmcraft_codecs::register_video_decoder`). Today that is VideoToolbox on macOS for H.264
//! (`avcC`) and HEVC (`hvcC`) streams, 8- and 10-bit, 4:2:0 and 4:2:2; on other systems
//! registration does nothing and reports [`Availability::Unavailable`].
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

mod annexb;
mod biplanar;
pub mod hybrid;
#[cfg(target_os = "windows")]
pub mod media_foundation;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod videotoolbox;

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
        filmcraft_codecs::hw::set_hw_backend("VideoToolbox");
        Availability::Available("VideoToolbox")
    }
    #[cfg(target_os = "windows")]
    {
        filmcraft_codecs::register_video_decoder(media_foundation_factory);
        filmcraft_codecs::hw::set_hw_backend("Media Foundation");
        Availability::Available("Media Foundation")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
        filmcraft_codecs::hw::NalStreamInfo::from_entry(entry).and_then(|r| r.ok()).is_some_and(|info| media_foundation::MfDecoder::new(info).is_ok())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = entry;
        false
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
/// `avcC` / `hvcC` streams a Direct3D-aware decoder MFT can decode with DXVA on this system's
/// GPU, `None` otherwise.
#[cfg(target_os = "windows")]
pub fn media_foundation_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry)?.ok()?;
    match media_foundation::MfDecoder::new(info.clone()) {
        Ok(mf) => Some(Ok(Box::new(HybridDecoder::new(Box::new(mf), entry.clone(), info)))),
        Err(why) => {
            log::info!("hardware decoding declined for {} video: {why}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}
