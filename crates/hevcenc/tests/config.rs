use filmcraft_hevcenc::{Encoder, EncoderConfig, Pass, PassStats, RateControl, YuvFrame};

fn frame_64() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    (vec![128u8; 64 * 64], vec![128u8; 32 * 32], vec![128u8; 32 * 32])
}

#[test]
fn defaults_are_main_ip() {
    let cfg = EncoderConfig::new(640, 360, 30, 1);
    let enc = Encoder::new(cfg).unwrap();
    assert_eq!(enc.delay(), 0);
    assert!(enc.pass_stats().is_none());
}

#[test]
fn rejects_bad_config() {
    assert!(Encoder::new(EncoderConfig::new(0, 16, 30, 1)).is_err());
    assert!(Encoder::new(EncoderConfig::new(15, 16, 30, 1)).is_err());
    assert!(Encoder::new(EncoderConfig::new(16, 16, 0, 1)).is_err());
    assert!(Encoder::new(EncoderConfig::new(16384 + 2, 16, 30, 1)).is_err());
    let mut bframes = EncoderConfig::new(64, 64, 30, 1);
    bframes.bframes = 1;
    assert!(Encoder::new(bframes).is_err());
    let mut keyint = EncoderConfig::new(64, 64, 30, 1);
    keyint.keyint = 0;
    assert!(Encoder::new(keyint).is_err());
    let mut qp = EncoderConfig::new(64, 64, 30, 1);
    qp.rate = RateControl::Qp(52);
    assert!(Encoder::new(qp).is_err());
    let mut crf = EncoderConfig::new(64, 64, 30, 1);
    crf.rate = RateControl::Crf(60.0);
    assert!(Encoder::new(crf).is_err());
    let mut cbr = EncoderConfig::new(64, 64, 30, 1);
    cbr.rate = RateControl::Cbr { kbps: 0 };
    assert!(Encoder::new(cbr).is_err());
    let mut level = EncoderConfig::new(64, 64, 30, 1);
    level.level = Some(99);
    assert!(Encoder::new(level).is_err());
    let mut pass = EncoderConfig::new(64, 64, 30, 1);
    pass.rate = RateControl::Cbr { kbps: 1000 };
    pass.pass = Pass::Second(PassStats::default());
    assert!(Encoder::new(pass).is_err());
}

#[test]
fn coding_encodes_a_flat_frame() {
    let mut enc = Encoder::new(EncoderConfig::new(64, 64, 30, 1)).unwrap();
    let (y, u, v) = frame_64();
    let frame = YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 };
    let packets = enc.encode(&frame, 0).unwrap();
    assert_eq!(packets.len(), 1);
    assert!(packets[0].keyframe);
    assert!(!packets[0].data.is_empty());
    assert!(enc.flush().is_empty());
    // Recon capture stays off unless enabled.
    assert!(enc.take_recon().is_empty());
}
