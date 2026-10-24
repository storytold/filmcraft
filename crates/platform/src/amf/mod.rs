//! AMD AMF hardware video encoding integration (Windows).
//!
//! Provides H.264, HEVC (Main / Main 10 HDR), and AV1 (RDNA 3+) encoding on AMD Radeon graphics.

pub mod device;
pub mod export;
#[allow(unsafe_code)]
pub mod ffi;
#[allow(unsafe_code)]
pub mod session;

use std::sync::{Once, OnceLock};

pub use session::{Codec, Config, Packet, Profile, Session};

static HEVC_AVAILABLE: OnceLock<bool> = OnceLock::new();
static HEVC_HDR_AVAILABLE: OnceLock<bool> = OnceLock::new();
static AV1_AVAILABLE: OnceLock<bool> = OnceLock::new();
static AV1_HDR_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Whether this system has an AMD GPU adapter present.
pub fn has_device() -> bool {
    device::Device::has_hardware_device()
}

/// Whether AMD AMF hardware encoding is available on this system.
pub fn is_available() -> bool {
    hevc_available() || av1_available()
}

/// Whether this system has an AMD GPU with hardware HEVC encoding.
pub fn hevc_available() -> bool {
    *HEVC_AVAILABLE.get_or_init(|| {
        let probe =
            Config { width: 640, height: 360, fps: (30, 1), bitrate_kbps: 2000, max_bitrate_kbps: 3000, cbr: false, keyint: 30, profile: Profile::HevcMain };
        let res = Session::open(&probe);
        if let Err(why) = &res {
            log::info!("no AMD AMF HEVC encoder: {why}");
        }
        res.is_ok()
    })
}

/// Whether this system has an AMD GPU with hardware HEVC Main 10 (HDR) encoding.
pub fn hevc_hdr_available() -> bool {
    *HEVC_HDR_AVAILABLE.get_or_init(|| {
        let probe =
            Config { width: 640, height: 360, fps: (30, 1), bitrate_kbps: 2000, max_bitrate_kbps: 3000, cbr: false, keyint: 30, profile: Profile::HevcMain10 };
        let res = Session::open(&probe);
        if let Err(why) = &res {
            log::info!("no AMD AMF HEVC Main 10 encoder: {why}");
        }
        res.is_ok()
    })
}

/// Whether this system has an AMD GPU with hardware AV1 encoding.
pub fn av1_available() -> bool {
    *AV1_AVAILABLE.get_or_init(|| {
        let probe =
            Config { width: 640, height: 360, fps: (30, 1), bitrate_kbps: 2000, max_bitrate_kbps: 3000, cbr: false, keyint: 30, profile: Profile::Av1Main };
        let res = Session::open(&probe);
        if let Err(why) = &res {
            log::info!("no AMD AMF AV1 encoder: {why}");
        }
        res.is_ok()
    })
}

/// Whether this system has an AMD GPU with hardware AV1 Main 10 (HDR) encoding.
pub fn av1_hdr_available() -> bool {
    *AV1_HDR_AVAILABLE.get_or_init(|| {
        let probe =
            Config { width: 640, height: 360, fps: (30, 1), bitrate_kbps: 2000, max_bitrate_kbps: 3000, cbr: false, keyint: 30, profile: Profile::Av1Main10 };
        let res = Session::open(&probe);
        if let Err(why) = &res {
            log::info!("no AMD AMF AV1 Main 10 encoder: {why}");
        }
        res.is_ok()
    })
}

/// Warm the HEVC probes ahead of time in a background thread.
pub fn warm_hevc_probe() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new().name("amf-hevc-probe".into()).spawn(|| {
            let _ = std::panic::catch_unwind(hevc_available);
            let _ = std::panic::catch_unwind(hevc_hdr_available);
        });
        if let Err(e) = spawned {
            log::info!("AMD AMF HEVC probe thread not started: {e}");
        }
    });
}

/// Warm the AV1 probes ahead of time in a background thread.
pub fn warm_av1_probe() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new().name("amf-av1-probe".into()).spawn(|| {
            let _ = std::panic::catch_unwind(av1_available);
            let _ = std::panic::catch_unwind(av1_hdr_available);
        });
        if let Err(e) = spawned {
            log::info!("AMD AMF AV1 probe thread not started: {e}");
        }
    });
}
