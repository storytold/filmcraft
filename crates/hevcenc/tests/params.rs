use filmcraft_bitstream::unescape_rbsp;
use filmcraft_hevc::Decoder;
use filmcraft_hevc::params::{Pps, Sps, Vps};
use filmcraft_hevcenc::{Encoder, EncoderConfig, RateControl};

fn sets(w: u32, h: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let enc = Encoder::new(EncoderConfig::new(w, h, 30, 1)).unwrap();
    let (vps, sps, pps) = enc.parameter_sets();
    (vps.to_vec(), sps.to_vec(), pps.to_vec())
}

fn rbsp(nal: &[u8]) -> Vec<u8> {
    unescape_rbsp(&nal[2..])
}

#[test]
fn nal_headers_carry_type_layer_and_temporal_id() {
    let (vps, sps, pps) = sets(64, 64);
    assert_eq!(&vps[..2], &[0x40, 0x01]);
    assert_eq!(&sps[..2], &[0x42, 0x01]);
    assert_eq!(&pps[..2], &[0x44, 0x01]);
}

#[test]
fn our_decoder_reads_our_parameter_sets() {
    let (vps, sps, pps) = sets(640, 360);
    let vps = Vps::parse(&rbsp(&vps)).unwrap();
    assert_eq!(vps.ptl.profile_idc, 1);
    assert_eq!(vps.ptl.level_idc, 63);
    let sps = Sps::parse(&rbsp(&sps)).unwrap();
    assert_eq!(sps.ptl.profile_idc, 1);
    assert_eq!(sps.ptl.level_idc, 63);
    assert_eq!(sps.width, 640);
    assert_eq!(sps.height, 384);
    assert_eq!(sps.conf_win, (0, 0, 0, 12));
    assert_eq!(sps.chroma_format_idc, 1);
    assert_eq!((sps.bit_depth_luma, sps.bit_depth_chroma), (8, 8));
    assert_eq!(sps.log2_max_poc_lsb, 9);
    assert_eq!(sps.max_dec_pic_buffering, 5);
    assert_eq!(sps.max_num_reorder, 0);
    assert_eq!((sps.log2_min_cb, sps.log2_ctb), (3, 6));
    assert_eq!((sps.log2_min_tb, sps.log2_max_tb), (2, 5));
    assert!(!sps.scaling_list_enabled && !sps.amp && !sps.sao && !sps.pcm);
    assert_eq!(sps.st_rps.len(), 1);
    assert!(!sps.long_term_refs_present && !sps.temporal_mvp && sps.strong_intra_smoothing);
    let vui = sps.vui.unwrap();
    assert_eq!(vui.timing, Some((1, 30)));
    assert_eq!((vui.colour_primaries, vui.transfer_characteristics, vui.matrix_coefficients), (1, 1, 1));
    let pps = Pps::parse(&rbsp(&pps)).unwrap();
    assert_eq!(pps.init_qp, 23);
    assert_eq!((pps.num_ref_idx_l0_default, pps.num_ref_idx_l1_default), (1, 1));
    assert!(pps.cu_qp_delta_enabled && !pps.sign_data_hiding && !pps.tiles_enabled);
    assert!(!pps.weighted_pred && !pps.weighted_bipred && !pps.transquant_bypass);
}

#[test]
fn odd_sizes_use_the_conformance_window() {
    let (_, sps, _) = sets(322, 180);
    let sps = Sps::parse(&rbsp(&sps)).unwrap();
    assert_eq!(sps.width, 384);
    assert_eq!(sps.height, 192);
    assert_eq!(sps.conf_win, (0, 31, 0, 6));
}

#[test]
fn level_follows_size_rate_and_bitrate() {
    let level = |w, h, fps, rate| Encoder::new(EncoderConfig { rate, ..EncoderConfig::new(w, h, fps, 1) }).unwrap().level_idc();
    assert_eq!(level(320, 240, 30, RateControl::Crf(23.0)), 60);
    assert_eq!(level(1920, 1080, 30, RateControl::Crf(23.0)), 120);
    assert_eq!(level(1920, 1080, 60, RateControl::Crf(23.0)), 123);
    assert_eq!(level(3840, 2160, 30, RateControl::Crf(23.0)), 150);
    assert_eq!(level(1920, 1080, 30, RateControl::Cbr { kbps: 8000 }), 120);
    assert_eq!(level(1920, 1080, 30, RateControl::Cbr { kbps: 30000 }), 153);
    let mut cfg = EncoderConfig::new(320, 240, 30, 1);
    cfg.level = Some(123);
    assert_eq!(Encoder::new(cfg).unwrap().level_idc(), 123);
}

#[test]
fn hvcc_parses_in_our_decoder() {
    let enc = Encoder::new(EncoderConfig::new(320, 240, 30, 1)).unwrap();
    let hvcc = enc.hvcc();
    assert_eq!(hvcc[0], 1);
    assert_eq!(hvcc[1], 1);
    assert_eq!(&hvcc[2..6], &[0x40, 0x00, 0x00, 0x00]);
    assert_eq!(hvcc[12], 60);
    assert_eq!(&hvcc[13..15], &[0xF0, 0x00]);
    assert_eq!(hvcc[15], 0xFC);
    assert_eq!(hvcc[16], 0xFD);
    assert_eq!(hvcc[21], 0x0F);
    assert_eq!(hvcc[22], 3);
    let (vps, sps, pps) = enc.parameter_sets();
    let mut pos = 23;
    for (nal_type, nal) in [(32u8, vps), (33, sps), (34, pps)] {
        assert_eq!(hvcc[pos], 0x80 | nal_type);
        assert_eq!(u16::from_be_bytes([hvcc[pos + 1], hvcc[pos + 2]]), 1);
        let len = u16::from_be_bytes([hvcc[pos + 3], hvcc[pos + 4]]) as usize;
        assert_eq!(len, nal.len());
        assert_eq!(&hvcc[pos + 5..pos + 5 + len], nal);
        pos += 5 + len;
    }
    assert_eq!(pos, hvcc.len());
    Decoder::from_hvcc(&hvcc).unwrap();
}
