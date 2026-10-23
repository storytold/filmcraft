//! VA-API encode sessions on this machine's GPU: what the driver offers for H.264 and a session
//! that opens, takes a picture and is dropped cleanly. Skips without a VA-API H.264 encoder.
#![cfg(target_os = "linux")]

use filmcraft_platform::vaapi::ffi;
use filmcraft_platform::vaapi::va::{EncodeSession, encode_caps};

#[test]
fn h264_encode_caps_and_a_session() {
    let selected = [ffi::VAProfileH264High, ffi::VAProfileH264Main].into_iter().find_map(|profile| encode_caps(profile).ok().map(|caps| (profile, caps)));
    let (profile, caps) = match selected {
        Some(c) => c,
        None => {
            eprintln!("SKIPPED: no VA-API H.264 encoder");
            return;
        }
    };
    assert!(caps.rate_control & (ffi::VA_RC_CBR | ffi::VA_RC_VBR) != 0, "CBR or VBR: {caps:?}");
    if let Some((w, h)) = caps.max_size {
        assert!(w >= 1920 && h >= 1080, "max encode size {w}x{h}");
    }
    let rc = if caps.rate_control & ffi::VA_RC_VBR != 0 { ffi::VA_RC_VBR } else { ffi::VA_RC_CBR };
    let mut s = EncodeSession::new(profile, caps.entrypoint, rc, 1918, 1078, 2).expect("session");
    assert_eq!(s.coded_size(), (1920, 1088), "macroblock-aligned");
    assert_eq!(s.recon_count(), 2);
    assert!(s.recon(0).is_some() && s.recon(1).is_some() && s.recon(2).is_none());
    let (y, c) = (vec![100u8; 1918 * 1078], vec![128u8; 959 * 539]);
    s.upload(&y, &c, &c, 1918, 1078).expect("upload");
    assert!(s.upload(&y, &c, &c, 1922, 1078).is_err(), "a picture wider than the session");
    // unsupported profile entrypoint: an error, not a crash
    assert!(encode_caps(ffi::VAProfileHEVCMain10 + 1000).is_err());
}
