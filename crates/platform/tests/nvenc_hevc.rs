//! NVENC H.265 (HEVC Main) encoding (Windows, NVIDIA GPU with an HEVC encoder): the stream decodes
//! with our own HEVC decoder to pictures close to the source, keyframes and timestamps are right, the
//! samples carry no parameter sets and the `hvcC` describes the SPS it was built from. Skips
//! without an NVIDIA GPU / driver with HEVC encoding.
#![cfg(target_os = "windows")]

use filmcraft_bitstream::unescape_rbsp;
use filmcraft_hevc::params::Sps;
use filmcraft_isobmff::{HevcConfig, SampleEntry};
use filmcraft_platform::nvenc::{Codec, Config, Nvenc, Packet, Profile, annex_b_to_length_prefixed_for, hevc_available, nal_type};

/// A moving test picture: gradient background, a moving box, a little texture.
fn rgba(w: usize, h: usize, i: usize) -> Vec<u8> {
    let mut v = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let (bx, by) = ((i * 7) % (w - 64), (i * 3) % (h - 64));
            let in_box = x >= bx && x < bx + 64 && y >= by && y < by + 64;
            let tex = ((x * 31 + y * 17 + i * 5) % 23) as u8;
            v[o] = if in_box { 230 } else { (x * 200 / w) as u8 + tex };
            v[o + 1] = if in_box { 40 } else { (y * 200 / h) as u8 + tex };
            v[o + 2] = if in_box { 60 } else { 90 + tex };
            v[o + 3] = 255;
        }
    }
    v
}

fn config(w: u32, h: u32) -> Config {
    Config {
        width: w,
        height: h,
        fps: (24, 1),
        bitrate_kbps: 6000,
        max_bitrate_kbps: 9000,
        cbr: false,
        keyint: 24,
        profile: Profile::HevcMain,
        level: None,
        sar: None,
        bframes: true,
    }
}

/// NVENC sessions are a limited resource on consumer GPUs: one test at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

type Sources = Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>;

/// `None` (the test skips) only on a machine without an HEVC encoder; on one that has it, a
/// configuration NVENC refuses is a failure, not a skip.
fn encode_all(cfg: &Config, frames: usize) -> Option<(Nvenc, Vec<Packet>, Sources)> {
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC encoder");
        return None;
    }
    let mut enc = Nvenc::new(cfg).unwrap_or_else(|why| panic!("NVENC HEVC is available but refused {cfg:?}: {why}"));
    let (w, h) = (cfg.width as usize, cfg.height as usize);
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let (mut packets, mut sources) = (Vec::new(), Vec::new());
    for i in 0..frames {
        filmcraft_export::rgba_to_yuv420_8(&rgba(w, h, i), w, h, &mut y, &mut u, &mut v);
        sources.push((y.clone(), u.clone(), v.clone()));
        packets.extend(enc.encode(&y, &u, &v, i as u64).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    Some((enc, packets, sources))
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

/// The NAL unit types of a length-prefixed sample.
fn sample_nal_types(sample: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(len) = sample.get(pos..pos + 4) {
        let n = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        pos += 4;
        let nal = sample.get(pos..pos + n).expect("a NAL unit inside the sample");
        out.push(nal_type(Codec::Hevc, nal).expect("a NAL header"));
        pos += n;
    }
    assert_eq!(pos, sample.len(), "samples are made of whole NAL units");
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn the_stream_decodes_to_the_source() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (w, h) = (1280u32, 720u32);
    let Some((enc, packets, sources)) = encode_all(&config(w, h), 72) else { return };
    assert_eq!(enc.codec(), Codec::Hevc);
    assert_eq!(packets.len(), 72, "one packet per picture");

    // timestamps: decoding order, presentation = frame number, dts = k - delay
    let delay = i64::from(enc.delay());
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(p.dts, k as i64 - delay, "dts of packet {k}");
        assert!(p.pts >= p.dts, "pts {} before dts {}", p.pts, p.dts);
    }
    assert!(packets.windows(2).all(|p| p[0].dts < p[1].dts), "dts strictly increases");
    let mut pts: Vec<i64> = packets.iter().map(|p| p.pts).collect();
    pts.sort_unstable();
    assert_eq!(pts, (0..72).collect::<Vec<_>>(), "every picture once");
    // keyframes every 24 pictures (IDR), nothing else flagged
    let keys: Vec<i64> = packets.iter().filter(|p| p.key).map(|p| p.pts).collect();
    assert_eq!(keys, vec![0, 24, 48], "IDR pictures");
    eprintln!("B-frame delay {delay}");

    // no parameter sets, delimiters or end markers in the samples; the keyframes are IDR slices
    for (k, p) in packets.iter().enumerate() {
        let types = sample_nal_types(&p.data);
        assert!(!types.is_empty(), "packet {k} has no NAL units");
        assert!(types.iter().all(|t| !(32..=37).contains(t)), "packet {k} carries NAL types {types:?}");
        if p.key {
            assert!(types.iter().any(|t| matches!(t, 19 | 20)), "keyframe {k}: {types:?}");
        }
    }

    // the record describes the SPS it came from
    let hvcc = enc.hevc_config().expect("an hvcC record").clone();
    let (sps_nal, vps_nal) = (enc.parameter_sets().0.to_vec(), enc.vps().to_vec());
    eprintln!("VPS {}\nSPS {}\nPPS {}", hex(&vps_nal), hex(&sps_nal), hex(enc.parameter_sets().1));
    let rbsp = unescape_rbsp(&sps_nal[2..]);
    let sps = Sps::parse(&rbsp).unwrap();
    // the coded size is a whole number of coding tree blocks; the conformance window crops it back
    assert!(sps.width >= w && sps.height >= h);
    let (_, _, shown_w, shown_h) = sps.crop_rect();
    assert_eq!((shown_w, shown_h), (w, h));
    assert!(sps.max_num_reorder <= enc.delay(), "SPS reorder {} but the dts shift is {}", sps.max_num_reorder, enc.delay());
    assert_eq!(&hvcc.to_bytes()[1..13], &rbsp[1..13], "the record's profile / tier / level bytes are the SPS's");
    assert_eq!(HevcConfig::parse(&hvcc.to_bytes()).unwrap(), hvcc);
    assert_eq!((hvcc.general_profile_idc, hvcc.general_tier_flag, hvcc.chroma_format_idc, hvcc.bit_depth_luma, hvcc.bit_depth_chroma), (1, false, 1, 8, 8));
    assert_eq!(usize::from(hvcc.num_temporal_layers), sps.max_sub_layers_minus1 as usize + 1);
    assert!(hvcc.arrays.iter().all(|a| a.completeness && a.nalus.len() == 1));
    assert_eq!(hvcc.arrays.iter().map(|a| a.nal_type).collect::<Vec<_>>(), vec![32, 33, 34]);
    eprintln!(
        "hvcC: level {} compat {:#010x} constraints {:#014x} nested {} layers {}",
        hvcc.general_level_idc,
        hvcc.general_profile_compatibility_flags,
        hvcc.general_constraint_indicator_flags,
        hvcc.temporal_id_nested,
        hvcc.num_temporal_layers
    );
    // BT.709 limited range, timing of the frame rate (a frame count per tick, not fields)
    let vui = sps.vui.clone().expect("a VUI");
    assert_eq!((vui.colour_primaries, vui.transfer_characteristics, vui.matrix_coefficients, vui.full_range), (1, 1, 1, false));
    assert_eq!(vui.timing, Some((1, 24)), "num_units_in_tick, time_scale");

    // our own decoder decodes the samples
    let entry = SampleEntry::hevc(hvcc, w as u16, h as u16);
    let mut dec = filmcraft_codecs::software_video_decoder(&entry).unwrap();
    let mut out = Vec::new();
    for p in &packets {
        out.extend(dec.decode(&p.data, p.pts).unwrap());
    }
    out.extend(dec.flush());
    assert_eq!(out.len(), 72);
    let mut worst = 99.0f64;
    let mut worst_chroma = 99.0f64;
    for f in &out {
        let src = &sources[f.pts as usize];
        let filmcraft_frame::PixelData::Yuv8 { planes, .. } = &f.frame.data else { panic!("8-bit planes") };
        worst = worst.min(psnr(&planes[0], &src.0));
        worst_chroma = worst_chroma.min(psnr(&planes[1], &src.1)).min(psnr(&planes[2], &src.2));
    }
    eprintln!("worst luma PSNR {worst:.1} dB, worst chroma {worst_chroma:.1} dB, {} bytes", packets.iter().map(|p| p.data.len()).sum::<usize>());
    assert!(worst > 43.0, "luma PSNR {worst:.1} dB");
    assert!(worst_chroma > 45.0, "chroma PSNR {worst_chroma:.1} dB");
}

#[test]
fn nothing_to_encode_with_is_an_error_not_a_crash() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC here");
        return;
    }
    // sizes NVENC cannot take, a zero frame rate, an HEVC level that does not exist
    for (w, h) in [(0, 0), (641, 360), (640, 361), (16, 16), (u32::MAX, u32::MAX), (100_000, 64)] {
        assert!(Nvenc::new(&config(w, h)).is_err(), "{w}x{h}");
    }
    let mut c = config(640, 360);
    c.fps = (0, 1);
    assert!(Nvenc::new(&c).is_err());
    c.fps = (24, 0);
    assert!(Nvenc::new(&c).is_err());
    c = config(640, 360);
    c.level = Some(42);
    assert!(Nvenc::new(&c).is_err());
    // a level that exists is taken
    c.level = Some(41);
    let enc = Nvenc::new(&c).expect("level 4.1");
    assert_eq!(enc.hevc_config().map(|h| h.general_level_idc), Some(123));
    // pictures smaller than the encoder's size
    let mut enc = Nvenc::new(&config(640, 360)).unwrap();
    assert!(enc.encode(&[0; 10], &[0; 10], &[0; 10], 0).is_err());
}

#[test]
fn hostile_annex_b_never_panics() {
    for s in [&[][..], &[0, 0, 1], &[1, 2, 3], &[0, 0, 1, 0x46, 0x01, 0x50], &[0, 0, 0, 1, 0x40], &[0, 0, 0, 1, 0x40, 0x01]] {
        assert!(annex_b_to_length_prefixed_for(Codec::Hevc, s).is_err(), "{s:?}");
    }
}

/// Run `f`, failing the test (not the process) if it panics.
fn calm<T>(what: &str, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => panic!("{what}: panicked"),
    }
}

#[test]
fn hostile_configurations_give_errors_not_crashes() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC here");
        return;
    }
    let base = config(640, 360);
    let variants: Vec<(&str, Config)> = vec![
        ("zero bitrate", Config { bitrate_kbps: 0, max_bitrate_kbps: 0, ..base.clone() }),
        ("huge bitrate", Config { bitrate_kbps: u32::MAX, max_bitrate_kbps: u32::MAX, ..base.clone() }),
        ("max below target", Config { bitrate_kbps: 6000, max_bitrate_kbps: 1, ..base.clone() }),
        ("zero cbr", Config { cbr: true, bitrate_kbps: 0, ..base.clone() }),
        ("cbr", Config { cbr: true, bitrate_kbps: 4000, ..base.clone() }),
        ("zero keyint", Config { keyint: 0, ..base.clone() }),
        ("keyint 2", Config { keyint: 2, ..base.clone() }),
        ("huge keyint", Config { keyint: u32::MAX, ..base.clone() }),
        ("keyint 1 (all intra)", Config { keyint: 1, ..base.clone() }),
        ("huge frame rate", Config { fps: (u32::MAX, 1), ..base.clone() }),
        ("huge frame duration", Config { fps: (1, u32::MAX), ..base.clone() }),
        ("huge aspect ratio", Config { sar: Some((u32::MAX, u32::MAX)), ..base.clone() }),
        ("zero aspect ratio", Config { sar: Some((0, 0)), ..base.clone() }),
        ("no B-frames", Config { bframes: false, ..base.clone() }),
        ("smallest size", Config { width: 130, height: 34, ..base.clone() }),
        ("odd size", Config { width: 321, height: 181, ..base.clone() }),
    ];
    for (name, c) in variants {
        match calm(name, || Nvenc::new(&c)) {
            Ok(mut enc) => {
                let (w, h) = (c.width as usize, c.height as usize);
                let (cw, ch) = (w / 2, h / 2);
                assert!(calm(name, || enc.encode(&[], &[], &[], 0)).is_err() || w == 0, "{name}: empty planes");
                assert!(calm(name, || enc.encode(&vec![0; w * h - 1], &vec![128; cw * ch], &vec![128; cw * ch], 0)).is_err(), "{name}: short luma");
                // a few real pictures, then the end; whatever comes out, nothing panics
                for i in 0..3u8 {
                    let _ = calm(name, || enc.encode(&vec![16 + i; w * h], &vec![128; cw * ch], &vec![128; cw * ch], i as u64));
                }
                let _ = calm(name, || enc.flush());
            }
            Err(why) => eprintln!("{name}: declined ({why})"),
        }
    }
}

#[test]
fn an_encoder_dropped_mid_stream_releases_everything() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC here");
        return;
    }
    // more sessions than a consumer GPU allows at once would fail the later ones: drop each early
    for round in 0..12 {
        let mut enc = Nvenc::new(&config(640, 360)).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..5 {
            filmcraft_export::rgba_to_yuv420_8(&rgba(640, 360, i), 640, 360, &mut y, &mut u, &mut v);
            enc.encode(&y, &u, &v, i as u64).unwrap();
        }
        // dropped with pictures in flight and no flush
    }
}

#[test]
fn high_bitrates_move_up_a_level_and_stay_in_the_main_tier() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // 1080p30 at a 12 / 18 Mbit/s peak: above level 4's Main tier (12), where NVENC's own choice was
    // level 4 High tier, which many hardware decoders refuse
    for (w, h, fps, kbps, max_kbps, level_idc) in
        [(1920u32, 1080u32, 30u32, 12_000u32, 18_000u32, 123u8), (1920, 1080, 30, 4_000, 6_000, 120), (1280, 720, 30, 5_000, 7_500, 93)]
    {
        let cfg = Config { fps: (fps, 1), bitrate_kbps: kbps, max_bitrate_kbps: max_kbps, keyint: 30, ..config(w, h) };
        let Some((enc, packets, _)) = encode_all(&cfg, 4) else { return };
        assert_eq!(packets.len(), 4);
        let record = enc.hevc_config().expect("an hvcC record");
        assert!(!record.general_tier_flag, "{w}x{h} at {max_kbps} kbit/s: Main tier");
        assert_eq!(record.general_level_idc, level_idc, "{w}x{h} at {max_kbps} kbit/s");
    }
}

#[test]
fn a_keyframe_every_one_or_two_pictures_is_written_without_b_frames() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // NVENC wants a GOP longer than the B-frame pattern: short GOPs drop the B-frames, they do not fail
    for keyint in [1u32, 2] {
        let cfg = Config { keyint, ..config(640, 360) };
        let Some((enc, packets, _)) = encode_all(&cfg, 12) else { return };
        assert_eq!(enc.delay(), 0, "keyint {keyint}");
        assert_eq!(packets.len(), 12);
        assert!(packets.iter().enumerate().all(|(k, p)| p.pts == k as i64 && p.dts == p.pts), "keyint {keyint}: no reordering");
        let keys: Vec<i64> = packets.iter().filter(|p| p.key).map(|p| p.pts).collect();
        assert_eq!(keys, (0..12).step_by(keyint as usize).collect::<Vec<i64>>(), "keyint {keyint}");
        let entry = SampleEntry::hevc(enc.hevc_config().expect("an hvcC record").clone(), 640, 360);
        let mut dec = filmcraft_codecs::software_video_decoder(&entry).unwrap();
        let mut n = 0;
        for p in &packets {
            n += dec.decode(&p.data, p.pts).unwrap().len();
        }
        assert_eq!(n + dec.flush().len(), 12, "keyint {keyint}");
    }
}
