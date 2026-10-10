//! NVENC with packed RGBA input (Windows and Linux, NVIDIA GPU): the GPU's RGB → 4:2:0 conversion matches the
//! export's own BT.709 limited-range conversion (`filmcraft_export::rgba_to_yuv420_8`), for HEVC Main
//! and H.264. Skips without an NVIDIA GPU / driver.
#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_platform::nvenc::{Codec, Config, Nvenc, Packet, Profile, Signal, available, hevc_available};

/// NVENC sessions are a limited resource on consumer GPUs: one test at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A moving test picture: gradient background, a moving box, a little texture.
fn moving(w: usize, h: usize, i: usize) -> Vec<u8> {
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

fn solid(w: usize, h: usize, rgb: [u8; 3]) -> Vec<u8> {
    [rgb[0], rgb[1], rgb[2], 255].repeat(w * h)
}

fn config(w: u32, h: u32, profile: Profile) -> Config {
    Config {
        width: w,
        height: h,
        fps: (30, 1),
        bitrate_kbps: 8000,
        max_bitrate_kbps: 12000,
        cbr: false,
        keyint: 30,
        profile,
        level: None,
        sar: None,
        bframes: true,
    }
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn mean(p: &[u8]) -> f64 {
    p.iter().map(|v| f64::from(*v)).sum::<f64>() / p.len() as f64
}

/// Encode `frames` through the RGBA input, decode with our own decoder: the decoded planes by picture.
fn round_trip(profile: Profile, w: usize, h: usize, frames: &[Vec<u8>]) -> Vec<[Vec<u8>; 3]> {
    let mut enc =
        Nvenc::with_rgba_input(&config(w as u32, h as u32, profile), &Signal::default()).unwrap_or_else(|e| panic!("{profile:?} with RGBA input: {e}"));
    assert!(enc.takes_rgba());
    assert!(enc.encode(&[0; 4], &[0; 1], &[0; 1], 0).is_err(), "planar pictures are refused");
    let mut packets: Vec<Packet> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        packets.extend(enc.encode_rgba(f, i as u64).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    assert_eq!(packets.len(), frames.len(), "one packet per picture");
    let entry = match enc.codec() {
        Codec::Hevc => SampleEntry::hevc(enc.hevc_config().expect("an hvcC record").clone(), w as u16, h as u16),
        Codec::H264 => {
            let (sps, pps) = enc.parameter_sets();
            SampleEntry::avc(AvcConfig::new(vec![sps.to_vec()], vec![pps.to_vec()], 4), w as u16, h as u16)
        }
    };
    let mut dec = filmcraft_codecs::software_video_decoder(&entry).unwrap();
    let mut out = Vec::new();
    for p in &packets {
        out.extend(dec.decode(&p.data, p.pts).unwrap());
    }
    out.extend(dec.flush());
    out.sort_by_key(|f| f.pts);
    assert_eq!(out.len(), frames.len());
    out.into_iter()
        .map(|f| {
            let filmcraft_frame::PixelData::Yuv8 { planes, .. } = &f.frame.data else { panic!("8-bit planes") };
            [planes[0].to_vec(), planes[1].to_vec(), planes[2].to_vec()]
        })
        .collect()
}

fn check(profile: Profile) {
    let (w, h) = (1280usize, 720usize);
    // solid colours: the decoded means are the BT.709 limited-range codes of the export's own conversion
    let colours: [[u8; 3]; 6] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255], [0, 0, 0], [128, 128, 128]];
    let mut frames: Vec<Vec<u8>> = colours.iter().map(|c| solid(w, h, *c)).collect();
    frames.extend((0..24).map(|i| moving(w, h, i)));
    let decoded = round_trip(profile, w, h, &frames);
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    for (k, c) in colours.iter().enumerate() {
        filmcraft_export::rgba_to_yuv420_8(&frames[k], w, h, &mut y, &mut u, &mut v);
        let want = [f64::from(y[0]), f64::from(u[0]), f64::from(v[0])];
        let got = [mean(&decoded[k][0]), mean(&decoded[k][1]), mean(&decoded[k][2])];
        eprintln!("{profile:?} {c:?}: Y/Cb/Cr {got:.1?}, BT.709 limited {want:?}");
        for (g, e) in got.iter().zip(want) {
            assert!((g - e).abs() <= 2.0, "{profile:?} {c:?}: decoded {got:.1?}, expected {want:?}");
        }
    }
    // a moving picture: close to the export's own conversion encoded the planar way
    let (mut worst, mut worst_chroma) = (99.0f64, 99.0f64);
    for (k, f) in frames.iter().enumerate().skip(colours.len()) {
        filmcraft_export::rgba_to_yuv420_8(f, w, h, &mut y, &mut u, &mut v);
        worst = worst.min(psnr(&decoded[k][0], &y));
        worst_chroma = worst_chroma.min(psnr(&decoded[k][1], &u)).min(psnr(&decoded[k][2], &v));
    }
    eprintln!("{profile:?}: worst luma PSNR {worst:.1} dB, chroma {worst_chroma:.1} dB");
    assert!(worst > 40.0, "{profile:?} luma PSNR {worst:.1} dB");
    assert!(worst_chroma > 40.0, "{profile:?} chroma PSNR {worst_chroma:.1} dB");
}

#[test]
fn hevc_rgba_input_converts_like_the_export() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC encoder");
        return;
    }
    check(Profile::HevcMain);
}

#[test]
fn h264_rgba_input_converts_like_the_export() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !available() {
        eprintln!("SKIPPED: no NVENC");
        return;
    }
    check(Profile::High);
}

#[test]
fn main10_has_no_rgba_input_and_short_pictures_are_errors() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_available() {
        eprintln!("SKIPPED: no NVENC HEVC encoder");
        return;
    }
    assert!(Nvenc::with_rgba_input(&config(640, 360, Profile::HevcMain10), &Signal::default()).is_err());
    let mut enc = Nvenc::with_rgba_input(&config(640, 360, Profile::HevcMain), &Signal::default()).unwrap();
    assert!(enc.encode_rgba(&[], 0).is_err());
    assert!(enc.encode_rgba(&vec![0; 640 * 360 * 4 - 1], 0).is_err());
    let mut planar = Nvenc::new(&config(640, 360, Profile::HevcMain)).unwrap();
    assert!(!planar.takes_rgba());
    assert!(planar.encode_rgba(&solid(640, 360, [1, 2, 3]), 0).is_err(), "RGBA pictures for a planar encoder");
}
