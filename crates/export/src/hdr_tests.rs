//! HDR plumbing shared by hardware encoders: the 10-bit 4:2:0 converter, the HDR probe registry, the
//! decision whether an export is HDR, and the one definition of the HDR10 static metadata.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const BT2020: (f32, f32) = (0.2627, 0.0593);

fn convert(rgb: &[f32], w: usize, h: usize) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    rgbf_to_yuv420_10(rgb, w, h, BT2020.0, BT2020.1, &mut y, &mut u, &mut v).unwrap();
    (y, u, v)
}

fn flat(w: usize, h: usize, px: [f32; 3]) -> Vec<f32> {
    px.iter().copied().cycle().take(w * h * 3).collect()
}

#[test]
fn black_white_and_grey_have_the_limited_range_codes() {
    for (px, code) in [([0.0; 3], 64u16), ([1.0; 3], 940), ([0.5; 3], 502)] {
        let (y, u, v) = convert(&flat(4, 4, px), 4, 4);
        assert!(y.iter().all(|c| *c == code), "{px:?}: {y:?}");
        assert!(u.iter().chain(&v).all(|c| *c == 512), "{px:?}: neutral chroma");
        assert_eq!((y.len(), u.len(), v.len()), (16, 4, 4));
    }
}

#[test]
fn bt2020_primaries_have_exact_codes() {
    // (R', G', B') -> (Y, Cb, Cr) with the BT.2020 non-constant-luminance matrix, 10-bit limited range
    for (px, want) in [
        ([1.0, 0.0, 0.0], (294u16, 387u16, 960u16)),
        ([0.0, 1.0, 0.0], (658, 189, 100)),
        ([0.0, 0.0, 1.0], (116, 960, 476)),
        ([1.0, 0.5, 0.0], (591, 225, 754)),
    ] {
        let (y, u, v) = convert(&flat(2, 2, px), 2, 2);
        assert_eq!((y[0], u[0], v[0]), want, "{px:?}");
    }
}

#[test]
fn out_of_range_and_non_finite_values_clamp_instead_of_wrapping() {
    // above 1, below 0 and infinities clamp to the legal range; NaN counts as 0 (black, not code 0)
    let mut px = vec![0.0f32; 4 * 3];
    px[..3].copy_from_slice(&[2.0, 2.0, 2.0]);
    px[3..6].copy_from_slice(&[-1.0, -5.0, -0.5]);
    px[6..9].copy_from_slice(&[f32::INFINITY, f32::INFINITY, f32::INFINITY]);
    px[9..].copy_from_slice(&[f32::NAN, f32::NEG_INFINITY, f32::NAN]);
    let (y, u, v) = convert(&px, 4, 1);
    assert_eq!(y, vec![940, 64, 940, 64]);
    assert!(u.iter().chain(&v).all(|c| (64..=960).contains(c)), "{u:?} {v:?}");
    // all NaN: black
    let (y, u, v) = convert(&[f32::NAN; 2 * 2 * 3], 2, 2);
    assert_eq!((y, u, v), (vec![64; 4], vec![512], vec![512]));
}

#[test]
fn odd_sizes_average_the_pixels_they_have() {
    // 3x3: the chroma planes are 2x2; the right column and bottom row average one or two pixels
    let (y, u, v) = convert(&flat(3, 3, [1.0, 0.0, 0.0]), 3, 3);
    assert_eq!((y.len(), u.len(), v.len()), (9, 4, 4));
    assert!(y.iter().all(|c| *c == 294) && u.iter().all(|c| *c == 387) && v.iter().all(|c| *c == 960));
    // 1x1
    let (y, u, v) = convert(&[0.0, 0.0, 1.0], 1, 1);
    assert_eq!((y, u, v), (vec![116], vec![960], vec![476]));
    // chroma is the mean over the 2x2 block: red and blue pixels give the mean of their Cb and Cr
    let rgb = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    let (y, u, v) = convert(&rgb, 2, 2);
    assert_eq!(y, vec![294, 116, 294, 116]);
    assert_eq!((u[0], v[0]), ((387u16 + 960) / 2, (960u16 + 476) / 2));
}

#[test]
fn a_ramp_has_every_code_a_10_bit_picture_can_have() {
    // a smooth horizontal ramp: 877 distinct luma codes in 64..=940, far more than 8 bits hold
    let w = 1024usize;
    let rgb: Vec<f32> = (0..w).flat_map(|x| [x as f32 / (w - 1) as f32; 3]).collect();
    let (y, _, _) = convert(&rgb, w, 1);
    let mut codes: Vec<u16> = y.clone();
    codes.dedup();
    assert_eq!((y[0], y[w - 1]), (64, 940));
    assert_eq!(codes.len(), 877, "{} distinct codes", codes.len());
    assert!(y.windows(2).all(|p| p[0] <= p[1]));
}

#[test]
fn hostile_input_is_an_error_not_a_panic() {
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let mut run = |rgb: &[f32], w: usize, h: usize| rgbf_to_yuv420_10(rgb, w, h, 0.2627, 0.0593, &mut y, &mut u, &mut v);
    assert!(run(&[], 0, 0).is_err());
    assert!(run(&[0.0; 3], 0, 1).is_err());
    assert!(run(&[0.0; 3], 1, 0).is_err());
    assert!(run(&[0.0; 11], 2, 2).is_err(), "short");
    assert!(run(&[0.0; 13], 2, 2).is_err(), "long");
    assert!(run(&[0.0; 6], 2, 2).is_err());
    assert!(run(&[0.0; 3], usize::MAX, 2).is_err(), "w*h overflows");
    assert!(run(&[0.0; 3], usize::MAX / 2, 3).is_err(), "w*h*3 overflows");
    assert!(run(&[0.0; 3], 1 << 40, 1 << 40).is_err());
}

#[test]
fn the_hdr_probe_registry_follows_its_probes() {
    static HERE: AtomicBool = AtomicBool::new(false);
    fn probe() -> bool {
        HERE.load(Ordering::SeqCst)
    }
    // a format nothing registered a probe for is never HDR; registering twice is harmless
    assert!(!hdr_available(Format::ProRes));
    register_hdr_probe(Format::ProRes, probe);
    register_hdr_probe(Format::ProRes, probe);
    assert!(!hdr_available(Format::ProRes));
    HERE.store(true, Ordering::SeqCst);
    assert!(hdr_available(Format::ProRes));
    assert!(!hdr_available(Format::DnxHr), "probes are per format");
    HERE.store(false, Ordering::SeqCst);
    assert!(!hdr_available(Format::ProRes));
}

#[test]
fn which_exports_are_hdr() {
    use crate::job::hdr_output;
    let never = |_: Format| false;
    let hevc_only = |f: Format| f == Format::Hevc;
    // H.264, ProRes, DNxHR and APV follow the working space, whatever the probes say
    for f in [Format::H264, Format::ProRes, Format::DnxHr, Format::Apv] {
        assert!(hdr_output(true, false, f, never), "{f:?}");
        assert!(!hdr_output(false, false, f, hevc_only), "{f:?}: SDR working space");
        assert!(!hdr_output(true, true, f, hevc_only), "{f:?}: SDR asked for");
    }
    // H.265 only where a registered encoder writes HDR
    assert!(!hdr_output(true, false, Format::Hevc, never), "no probe: tone-mapped SDR, as with VideoToolbox");
    assert!(hdr_output(true, false, Format::Hevc, hevc_only));
    assert!(!hdr_output(true, true, Format::Hevc, hevc_only), "settings.sdr forces SDR");
    assert!(!hdr_output(false, false, Format::Hevc, hevc_only));
    // formats without HDR output
    assert!(!hdr_output(true, false, Format::Mjpeg, |_| true));
}

#[test]
fn only_pq_has_static_metadata_and_it_is_what_h264_writes() {
    assert!(ColorSignal::HLG.static_metadata().is_none());
    assert!(ColorSignal::default().static_metadata().is_none());
    let (md, cll) = ColorSignal::PQ.static_metadata().unwrap();
    assert_eq!(cll, (0, 0));
    assert_eq!(md, filmcraft_isobmff::MasteringDisplay::bt2020(1000.0, 0.0001));
    assert_eq!(md.to_bytes().len(), 24);
    // the box on the sample entry is the same value
    let mut e = filmcraft_isobmff::SampleEntry::hevc(Default::default(), 64, 64);
    ColorSignal::PQ.apply_to(&mut e, false);
    let v = e.video.as_ref().unwrap();
    assert_eq!((v.mastering_display, v.content_light), (Some(md), Some((0, 0))));
    let mut e = filmcraft_isobmff::SampleEntry::hevc(Default::default(), 64, 64);
    ColorSignal::HLG.apply_to(&mut e, false);
    let v = e.video.as_ref().unwrap();
    assert!(v.color.is_some() && v.mastering_display.is_none() && v.content_light.is_none());
}

/// Remove emulation-prevention bytes (`00 00 03` -> `00 00`).
fn unescape(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut zeros = 0;
    for &x in b {
        if zeros >= 2 && x == 3 {
            zeros = 0;
            continue;
        }
        zeros = if x == 0 { zeros + 1 } else { 0 };
        out.push(x);
    }
    out
}

#[test]
fn the_software_h264_sei_is_the_static_metadata_laid_out_as_messages() {
    let s = ExportSettings { format: Format::H264, signal: ColorSignal::PQ, ..Default::default() };
    let mut enc = h264_factory(Format::H264, 64, 64, FrameRate::FPS_24, &s).unwrap().unwrap();
    let rgb = vec![0.5f32; 64 * 64 * 3];
    let mut packets = enc.encode(&EncoderFrame { width: 64, height: 64, rgba: &[], hdr: Some(&rgb), index: 0 }).unwrap();
    packets.extend(enc.flush().unwrap());
    // find the SEI NAL unit (type 6) of the length-prefixed first access unit
    let data = &packets[0].data;
    let (mut pos, mut sei) = (0usize, None);
    while let Some(len) = data.get(pos..pos + 4) {
        let n = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let nal = &data[pos + 4..pos + 4 + n];
        if nal[0] & 0x1f == 6 {
            sei = Some(unescape(&nal[1..]));
        }
        pos += 4 + n;
    }
    let sei = sei.expect("an SEI NAL unit in the first access unit");
    let (md, (cll, fall)) = ColorSignal::PQ.static_metadata().unwrap();
    let mut want = vec![137u8, 24];
    want.extend(md.to_bytes());
    want.extend([144u8, 4]);
    want.extend(cll.to_be_bytes());
    want.extend(fall.to_be_bytes());
    want.push(0x80);
    assert_eq!(sei, want);
}
