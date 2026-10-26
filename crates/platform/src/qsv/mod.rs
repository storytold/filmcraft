//! Hardware video encoding on Intel GPUs (Intel Quick Sync Video) via Media Foundation transforms.
//!
//! Provides hardware-accelerated H.264, HEVC (Main and Main 10 HDR), and AV1 export on Intel
//! integrated (Iris Xe, UHD) and discrete graphics (Arc A-series, B-series).

pub mod device;
pub mod export;
pub mod session;

use std::sync::OnceLock;

#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{MFVideoFormat_AV1, MFVideoFormat_HEVC};

use self::device::IntelGpu;
#[cfg(target_os = "windows")]
use self::session::find_encoder_activate;
#[cfg(target_os = "windows")]
use crate::media_foundation::mft::api;

static HEVC_AVAILABLE: OnceLock<bool> = OnceLock::new();
static HEVC_HDR_AVAILABLE: OnceLock<bool> = OnceLock::new();
static AV1_AVAILABLE: OnceLock<bool> = OnceLock::new();
static AV1_HDR_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Whether an Intel GPU is physically present on this system.
pub fn has_device() -> bool {
    IntelGpu::probe().ok().flatten().is_some()
}

/// Whether Intel Quick Sync Video hardware encoding is available on this system.
pub fn is_available() -> bool {
    hevc_available() || av1_available()
}

/// Whether an Intel GPU is present and supports HEVC hardware export.
pub fn hevc_available() -> bool {
    *HEVC_AVAILABLE.get_or_init(|| probe_codec_hevc(false))
}

/// Whether an Intel GPU is present and supports HEVC Main 10 (10-bit / HDR) hardware export.
pub fn hevc_hdr_available() -> bool {
    *HEVC_HDR_AVAILABLE.get_or_init(|| probe_codec_hevc(true))
}

/// Whether an Intel GPU is present and supports AV1 hardware export.
pub fn av1_available() -> bool {
    *AV1_AVAILABLE.get_or_init(|| probe_codec_av1(false))
}

/// Whether an Intel GPU is present and supports AV1 Main 10 (10-bit / HDR) hardware export.
pub fn av1_hdr_available() -> bool {
    *AV1_HDR_AVAILABLE.get_or_init(|| probe_codec_av1(true))
}

/// Ask [`hevc_available`] on a thread of its own (once, however often this is called).
pub fn warm_hevc_probe() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new().name("qsv-hevc-probe".into()).spawn(|| {
            if std::panic::catch_unwind(hevc_available).is_err() {
                log::warn!("the hardware QSV HEVC probe panicked; the format list will ask again");
            }
            if std::panic::catch_unwind(hevc_hdr_available).is_err() {
                log::warn!("the hardware QSV HEVC Main 10 probe panicked; the export will ask again");
            }
        });
        if let Err(e) = spawned {
            log::info!("hardware QSV HEVC probe thread not started: {e}");
        }
    });
}

/// Ask [`av1_available`] on a thread of its own (once, however often this is called).
pub fn warm_av1_probe() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new().name("qsv-av1-probe".into()).spawn(|| {
            if std::panic::catch_unwind(av1_available).is_err() {
                log::warn!("the hardware QSV AV1 probe panicked; the format list will ask again");
            }
            if std::panic::catch_unwind(av1_hdr_available).is_err() {
                log::warn!("the hardware QSV AV1 Main 10 probe panicked; the export will ask again");
            }
        });
        if let Err(e) = spawned {
            log::info!("hardware QSV AV1 probe thread not started: {e}");
        }
    });
}

#[cfg(target_os = "windows")]
fn probe_codec_hevc(_ten_bit: bool) -> bool {
    probe_codec_win(MFVideoFormat_HEVC)
}

#[cfg(target_os = "windows")]
fn probe_codec_av1(_ten_bit: bool) -> bool {
    probe_codec_win(MFVideoFormat_AV1)
}

#[cfg(target_os = "windows")]
fn probe_codec_win(subtype: windows::core::GUID) -> bool {
    let Ok(Some(_gpu)) = IntelGpu::probe() else {
        return false;
    };
    let Ok(mf_api) = api() else {
        return false;
    };
    let Ok(Some(_)) = find_encoder_activate(mf_api, subtype) else {
        return false;
    };
    true
}

#[cfg(not(target_os = "windows"))]
fn probe_codec_hevc(_ten_bit: bool) -> bool {
    IntelGpu::probe().ok().flatten().is_some()
}

#[cfg(not(target_os = "windows"))]
fn probe_codec_av1(_ten_bit: bool) -> bool {
    let Ok(Some(gpu)) = IntelGpu::probe() else {
        return false;
    };
    let id = gpu.device_id;
    (0x5690..=0x56c2).contains(&id) || (0x7d40..=0x7d67).contains(&id) || (0x64a0..=0x64b0).contains(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_id_is_intel() {
        assert_eq!(device::INTEL_VENDOR_ID, 0x8086);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn codec_subtypes_are_distinct() {
        use session::Codec;
        assert_ne!(Codec::H264.subtype(), Codec::Hevc.subtype());
        assert_ne!(Codec::Hevc.subtype(), Codec::Av1.subtype());
    }

    #[test]
    fn codec_names() {
        use session::Codec;
        assert_eq!(Codec::H264.name(), "H.264");
        assert_eq!(Codec::Hevc.name(), "HEVC");
        assert_eq!(Codec::Av1.name(), "AV1");
    }
}
