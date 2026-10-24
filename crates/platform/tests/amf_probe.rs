//! AMD AMF hardware encoder integration tests.
#![cfg(target_os = "windows")]

use filmcraft_platform::amf::{Config, Profile, Session, av1_available, av1_hdr_available, hevc_available, hevc_hdr_available};

#[test]
fn test_amf_safe_session_h264() {
    let cfg = Config { width: 320, height: 240, fps: (30, 1), bitrate_kbps: 1000, max_bitrate_kbps: 1500, cbr: false, keyint: 30, profile: Profile::H264High };
    let mut session = match Session::open(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("AMF H.264 Session::open skipped/failed: {e}");
            return;
        }
    };
    let y = vec![128u8; 320 * 240];
    let u = vec![128u8; 160 * 120];
    let v = vec![128u8; 160 * 120];
    let mut total_packets = 0;
    for i in 0..5 {
        let pkts = session.encode_8bit(&y, &u, &v, i * 333_333, 333_333).unwrap();
        total_packets += pkts.len();
    }
    let drain_pkts = session.drain().unwrap();
    total_packets += drain_pkts.len();
    println!("Total H.264 packets from safe AMF Session: {total_packets}");
    assert!(total_packets >= 5);
}

#[test]
fn test_amf_safe_session_hevc() {
    let cfg = Config { width: 320, height: 240, fps: (30, 1), bitrate_kbps: 1000, max_bitrate_kbps: 1500, cbr: false, keyint: 30, profile: Profile::HevcMain };
    let mut session = match Session::open(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("AMF HEVC Session::open skipped/failed: {e}");
            return;
        }
    };
    let y = vec![128u8; 320 * 240];
    let u = vec![128u8; 160 * 120];
    let v = vec![128u8; 160 * 120];
    let mut total_packets = 0;
    for i in 0..5 {
        let pkts = session.encode_8bit(&y, &u, &v, i * 333_333, 333_333).unwrap();
        total_packets += pkts.len();
    }
    let drain_pkts = session.drain().unwrap();
    total_packets += drain_pkts.len();
    println!("Total HEVC packets from safe AMF Session: {total_packets}");
    assert!(total_packets >= 5);
}

#[test]
fn test_amf_probes() {
    println!("AMF hevc_available: {}", hevc_available());
    println!("AMF hevc_hdr_available: {}", hevc_hdr_available());
    println!("AMF av1_available: {}", av1_available());
    println!("AMF av1_hdr_available: {}", av1_hdr_available());
}
