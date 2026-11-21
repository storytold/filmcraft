//! Hardware video encoding on Intel GPUs (Intel Quick Sync Video) via Media Foundation transforms.
//!
//! Provides hardware-accelerated H.264, HEVC (Main and Main 10 HDR), and AV1 export on Intel
//! integrated (Iris Xe, UHD) and discrete graphics (Arc A-series, B-series).

pub(crate) mod device;
pub mod export;
pub(crate) mod session;

use std::sync::OnceLock;

use self::device::IntelGpu;
pub use self::device::{INTEL_VENDOR_ID, IntelAdapterInfo};
pub use self::session::{Codec, Profile};
#[cfg(target_os = "windows")]
use self::session::{Config, QsvSession};

static HEVC_AVAILABLE: OnceLock<bool> = OnceLock::new();
static HEVC_HDR_AVAILABLE: OnceLock<bool> = OnceLock::new();
static AV1_AVAILABLE: OnceLock<bool> = OnceLock::new();
static AV1_HDR_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Probe whether an Intel GPU adapter is present without allocating a D3D11 device.
pub fn probe_adapter() -> Result<Option<IntelAdapterInfo>, String> {
    IntelGpu::probe()
}

/// Whether an Intel GPU is physically present on this system.
pub fn has_device() -> bool {
    probe_adapter().ok().flatten().is_some()
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
fn probe_codec_hevc(ten_bit: bool) -> bool {
    let profile = if ten_bit { Profile::HevcMain10 } else { Profile::HevcMain };
    probe_codec_session(Codec::Hevc, profile, ten_bit)
}

#[cfg(target_os = "windows")]
fn probe_codec_av1(ten_bit: bool) -> bool {
    let profile = if ten_bit { Profile::Av1Main10 } else { Profile::Av1Main };
    probe_codec_session(Codec::Av1, profile, ten_bit)
}

#[cfg(target_os = "windows")]
fn probe_codec_session(codec: Codec, profile: Profile, ten_bit: bool) -> bool {
    let Ok(Some(_gpu)) = IntelGpu::probe() else {
        return false;
    };
    let cfg = Config { codec, profile, size: (640, 360), rate: filmcraft_time::FrameRate::FPS_24, bitrate_kbps: 2000, keyint: 60, ten_bit };
    match QsvSession::new(cfg) {
        Ok(_session) => true,
        Err(e) => {
            log::debug!("Intel Quick Sync probe ({codec:?}, ten_bit={ten_bit}) unavailable: {e}");
            false
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn probe_codec_hevc(_ten_bit: bool) -> bool {
    // Media Foundation QSV encoding is supported only on Windows;
    // non-Windows reports unavailable until a VA-API session backend is implemented.
    false
}

#[cfg(not(target_os = "windows"))]
fn probe_codec_av1(_ten_bit: bool) -> bool {
    // Media Foundation QSV encoding is supported only on Windows;
    // non-Windows reports unavailable until a VA-API session backend is implemented.
    false
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

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_windows_qsv_reports_unavailable_without_session_backend() {
        assert!(!hevc_available(), "QSV HEVC must be unavailable on non-Windows until a backend is implemented");
        assert!(!av1_available(), "QSV AV1 must be unavailable on non-Windows until a backend is implemented");
        assert!(!is_available());
    }
}
