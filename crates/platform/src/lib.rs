//! OS media integration (layer L5): hardware video decoding and encoding through the operating
//! system's codecs.
//!
//! [`register`] puts the platform's hardware decoder factory in front of FilmCraft's own decoders
//! (`filmcraft_codecs::register_video_decoder`). Today that is VideoToolbox on macOS for H.264
//! (`avcC`) and HEVC (`hvcC`) streams, 8- and 10-bit, 4:2:0 and 4:2:2; Media Foundation / DXVA on
//! Windows; NVDEC ([`nvdec`], NVIDIA's driver) and VA-API ([`vaapi`]) on Linux for H.264 and HEVC; on other systems (and on
//! Linux with neither) registration does nothing and reports [`Availability::Unavailable`]. It also registers a
//! hardware H.264 encoder factory (`filmcraft_export::register_encoder`) that only acts when an
//! export asks for it (`ExportSettings::hardware_encoding` = `Auto`), see [`hardware_encode`]; on
//! macOS and with NVENC ([`nvenc`], Windows and Linux) it also makes the H.265 export format available, and with NVENC
//! (Main 10) lets HDR sequences export as HDR H.265.
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

// Used by the Windows (and, for `biplanar`, Linux) decoders; compiled everywhere so their tests run
// on every system.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod annexb;
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
mod biplanar;
pub mod cursor;
#[cfg(target_os = "macos")]
pub mod hardware_encode;
pub mod hybrid;
#[cfg(target_os = "windows")]
pub mod media_foundation;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub mod nvdec;
#[cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]
pub mod nvenc;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
        // the probe creates a hardware session (about 0.1 s): answer it now, off the UI thread,
        // before the first draw of the format list asks
        hardware_encode::warm_hevc_probe();
        filmcraft_codecs::hw::set_hw_backend("VideoToolbox");
        Availability::Available("VideoToolbox")
    }
    #[cfg(target_os = "windows")]
    {
        register_nvenc();
        filmcraft_codecs::register_video_decoder(media_foundation_factory);
        filmcraft_codecs::hw::set_hw_backend("Media Foundation");
        Availability::Available("Media Foundation")
    }
    #[cfg(target_os = "linux")]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        let mut available = match vaapi::va::probe() {
            Ok(driver) => {
                ONCE.call_once(|| log::info!("hardware decoding through VA-API: {driver}"));
                filmcraft_codecs::register_video_decoder(vaapi_factory);
                filmcraft_codecs::hw::set_hw_backend("VA-API");
                Availability::Available("VA-API")
            }
            Err(why) => {
                log::info!("no VA-API hardware decoding: {why}");
                Availability::Unavailable("no VA-API driver (libva and a DRM render node) and no NVIDIA driver (NVDEC)")
            }
        };
        // NVIDIA's proprietary driver has no VA-API of its own: NVDEC goes in front, and what it
        // declines still reaches VA-API (a second GPU) and then the software decoders
        #[cfg(target_pointer_width = "64")]
        match nvdec::cuvid::probe() {
            Ok(driver) => {
                static NVDEC: std::sync::Once = std::sync::Once::new();
                NVDEC.call_once(|| log::info!("hardware decoding through NVDEC: {driver}"));
                filmcraft_codecs::register_video_decoder(nvdec_factory);
                filmcraft_codecs::hw::set_hw_backend("NVDEC");
                available = Availability::Available("NVDEC");
            }
            Err(why) => log::info!("no NVDEC hardware decoding: {why}"),
        }
        // After the VA-API probe, never beside it: libva-nvidia-driver calls `cuInit` from its
        // library constructor (under the loader lock) while a first `cuInit` on another thread
        // loads libraries under CUDA's own lock, and the two wait for each other forever.
        #[cfg(target_pointer_width = "64")]
        register_nvenc();
        available
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Availability::Unavailable("no hardware video decoder for this system yet")
    }
}

/// NVENC: the Export encoder factory, and what makes the H.265 format (and HDR H.265) available.
#[cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]
fn register_nvenc() {
    // Export ▸ H.265: NVENC is FilmCraft's only HEVC encoder on Windows and Linux, so the format is available
    // when a small HEVC session opens. That takes about half a second, so it is asked on a thread of
    // its own now rather than by the first draw of the format list.
    filmcraft_export::register_format_probe(filmcraft_export::Format::Hevc, nvenc::hevc_available);
    // HDR sequences export as HEVC Main 10 (PQ / HLG) only where NVENC has a 10-bit encoder; elsewhere
    // (and on macOS, whose VideoToolbox path is 8-bit) they are tone-mapped to SDR, as before
    filmcraft_export::register_hdr_probe(filmcraft_export::Format::Hevc, nvenc::hevc_hdr_available);
    nvenc::warm_hevc_probe();
    static ENCODERS: std::sync::Once = std::sync::Once::new();
    // Export ▸ Hardware encoding (NVENC H.264): in front of the software encoder, taking an
    // export only when asked for and when NVENC can do it
    ENCODERS.call_once(|| filmcraft_export::register_encoder(nvenc::export::factory));
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
        #[cfg(target_pointer_width = "64")]
        if filmcraft_codecs::video_decoder_registered(nvdec_factory) {
            return true;
        }
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
        filmcraft_codecs::hw::NalStreamInfo::from_entry(entry).and_then(|r| r.ok()).is_some_and(|info| {
            #[cfg(target_pointer_width = "64")]
            if nvdec::NvDecoder::new(info.clone()).is_ok() {
                return true;
            }
            vaapi::VaDecoder::new(info).is_ok()
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
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

/// The VA-API factory: a [`HybridDecoder`] around [`vaapi::VaDecoder`] for H.264 and HEVC
/// streams this system's VA-API driver decodes, `None` otherwise (other codecs keep the software
/// decoders).
#[cfg(target_os = "linux")]
pub fn vaapi_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry)?.ok()?;
    match vaapi::VaDecoder::new(info.clone()) {
        Ok(va) => Some(Ok(Box::new(HybridDecoder::new(Box::new(va), entry.clone(), info)))),
        Err(why) => {
            log::info!("hardware decoding declined for {} video: {why}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}

/// The NVDEC factory: a [`HybridDecoder`] around [`nvdec::NvDecoder`] for H.264 and HEVC streams
/// this system's NVIDIA GPU decodes, `None` otherwise (VA-API and the software decoders keep those).
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn nvdec_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry)?.ok()?;
    match nvdec::NvDecoder::new(info.clone()) {
        Ok(nv) => Some(Ok(Box::new(HybridDecoder::new(Box::new(nv), entry.clone(), info)))),
        Err(why) => {
            log::info!("NVDEC declined {} video: {why}", entry.codec.name());
            // with VA-API behind this factory, that one counts the decline (or takes the stream)
            if !filmcraft_codecs::video_decoder_registered(vaapi_factory) {
                filmcraft_codecs::hw::note_hw_declined();
            }
            None
        }
    }
}
