//! Intel Quick Sync Video encoder probe, export, and configuration tests.

#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

use filmcraft_export::{ExportSettings, Format, HardwareEncoding};
use filmcraft_platform::qsv::device::{INTEL_VENDOR_ID, IntelGpu};
use filmcraft_platform::qsv::export::factory;
use filmcraft_platform::qsv::session::Codec;
use filmcraft_time::FrameRate;

#[test]
fn intel_vendor_id_constant() {
    assert_eq!(INTEL_VENDOR_ID, 0x8086);
}

#[test]
fn probe_does_not_panic() {
    // Probing for an Intel GPU on any machine must return a Result and never panic
    let result = IntelGpu::probe();
    assert!(result.is_ok(), "Intel GPU probe failed: {:?}", result.err());
}

#[test]
fn codec_properties() {
    assert_eq!(Codec::H264.name(), "H.264");
    assert_eq!(Codec::Hevc.name(), "HEVC");
    assert_eq!(Codec::Av1.name(), "AV1");

    #[cfg(target_os = "windows")]
    {
        assert_ne!(Codec::H264.subtype(), Codec::Hevc.subtype());
        assert_ne!(Codec::Hevc.subtype(), Codec::Av1.subtype());
    }
}

#[test]
fn qsv_factory_declines_when_not_requested() {
    let s = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Off, ..Default::default() };
    let res = factory(Format::H264, 1920, 1080, FrameRate::new(30, 1), &s);
    assert!(res.is_none(), "factory should decline when hardware encoding is Off");
}

#[test]
fn qsv_factory_declines_unsupported_formats() {
    let s = ExportSettings { format: Format::ProRes, hardware_encoding: HardwareEncoding::Auto, ..Default::default() };
    let res = factory(Format::ProRes, 1920, 1080, FrameRate::new(30, 1), &s);
    assert!(res.is_none(), "factory should decline ProRes");
}

#[test]
fn qsv_probes_do_not_panic() {
    let _ = filmcraft_platform::qsv::hevc_available();
    let _ = filmcraft_platform::qsv::hevc_hdr_available();
    let _ = filmcraft_platform::qsv::av1_available();
    let _ = filmcraft_platform::qsv::av1_hdr_available();
    filmcraft_platform::qsv::warm_hevc_probe();
    filmcraft_platform::qsv::warm_av1_probe();
}
