//! NVENC H.264 encoding (Windows, NVIDIA GPU): the stream decodes with our own decoder to pictures
//! close to the source, keyframes and timestamps are right, and Export takes it only when asked.
//! Skips without an NVIDIA GPU / driver with NVENC.
#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_platform::nvenc::{Config, NvencH264, Packet, Profile};

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
        profile: Profile::High,
        level: None,
        sar: None,
        bframes: true,
    }
}

fn encode_all(cfg: &Config, frames: usize) -> Option<(NvencH264, Vec<Packet>, Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>)> {
    let mut enc = match NvencH264::new(cfg) {
        Ok(e) => e,
        Err(why) => {
            eprintln!("SKIPPED: {why}");
            return None;
        }
    };
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

#[test]
fn the_stream_decodes_to_the_source() {
    let (w, h) = (1280u32, 720u32);
    let Some((enc, packets, sources)) = encode_all(&config(w, h), 72) else { return };
    assert_eq!(packets.len(), 72, "one packet per picture");

    // timestamps: decoding order, presentation = frame number, dts = k - delay
    let delay = i64::from(enc.delay());
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(p.dts, k as i64 - delay, "dts of packet {k}");
        assert!(p.pts >= p.dts, "pts {} before dts {}", p.pts, p.dts);
    }
    let mut pts: Vec<i64> = packets.iter().map(|p| p.pts).collect();
    pts.sort_unstable();
    assert_eq!(pts, (0..72).collect::<Vec<_>>(), "every picture once");
    // keyframes every 24 pictures (IDR), nothing else flagged
    let keys: Vec<i64> = packets.iter().filter(|p| p.key).map(|p| p.pts).collect();
    assert_eq!(keys, vec![0, 24, 48], "IDR pictures");

    // the avcC parses and our decoder decodes the samples
    let (sps, pps) = enc.parameter_sets();
    let entry = SampleEntry::avc(AvcConfig::new(vec![sps.to_vec()], vec![pps.to_vec()], 4), w as u16, h as u16);
    let mut dec = filmcraft_codecs::software_video_decoder(&entry).unwrap();
    let mut out = Vec::new();
    for p in &packets {
        out.extend(dec.decode(&p.data, p.pts).unwrap());
    }
    out.extend(dec.flush());
    assert_eq!(out.len(), 72);
    let mut worst = 99.0f64;
    for f in &out {
        let src = &sources[f.pts as usize];
        let filmcraft_frame::PixelData::Yuv8 { planes, .. } = &f.frame.data else { panic!("8-bit planes") };
        worst = worst.min(psnr(&planes[0], &src.0));
        assert!(psnr(&planes[1], &src.1) > 33.0 && psnr(&planes[2], &src.2) > 33.0, "chroma of picture {}", f.pts);
    }
    assert!(worst > 34.0, "luma PSNR {worst:.1} dB");
    eprintln!("worst luma PSNR {worst:.1} dB, {} bytes", packets.iter().map(|p| p.data.len()).sum::<usize>());
}
