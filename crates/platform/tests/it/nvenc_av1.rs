//! NVENC AV1 Main encoding (Windows, NVIDIA GPU with an AV1 encoder): the stream decodes with our
//! own AV1 decoder to pictures close to the source, keyframes and timestamps are right, the samples
//! carry sized OBUs without temporal delimiters, and the `av1C` describes the sequence header it
//! was built from. Skips without an NVIDIA GPU / driver with AV1 encoding.
#![cfg(target_os = "windows")]

use craft_video::av1::decode::SequenceHeader;
use filmcraft_isobmff::{Av1Config, SampleEntry};
use filmcraft_platform::nvenc::{Codec, Config, Nvenc, Packet, Profile, av1_available};

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
        profile: Profile::Av1Main,
        level: None,
        sar: None,
        bframes: true,
    }
}

type Sources = Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>;

fn encode_all(cfg: &Config, frames: usize) -> Option<(Nvenc, Vec<Packet>, Sources)> {
    if !av1_available() {
        eprintln!("SKIPPED: no NVENC AV1 encoder");
        return None;
    }
    let mut enc = Nvenc::new(cfg).unwrap_or_else(|why| panic!("NVENC AV1 is available but refused {cfg:?}: {why}"));
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

/// Walk sized OBUs inside a sample and return their `obu_type` values.
fn sample_obu_types(sample: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < sample.len() {
        let h = sample[pos];
        assert_eq!(h & 0x80, 0, "forbidden bit");
        let obu_type = (h >> 3) & 0x0f;
        let has_ext = h & 0x04 != 0;
        let has_size = h & 0x02 != 0;
        assert!(has_size, "ISOBMFF av01 samples must have obu_has_size_field = 1");
        pos += 1;
        if has_ext {
            pos += 1;
        }
        let mut size = 0usize;
        let mut shift = 0u32;
        loop {
            let b = sample[pos];
            pos += 1;
            size |= usize::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        pos += size;
        assert!(pos <= sample.len(), "OBU overruns sample");
        out.push(obu_type);
    }
    assert_eq!(pos, sample.len());
    out
}

#[test]
fn the_stream_decodes_to_the_source() {
    let _one = crate::common::NVENC_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (w, h) = (1280u32, 720u32);
    let Some((enc, packets, sources)) = encode_all(&config(w, h), 72) else { return };
    assert_eq!(enc.codec(), Codec::Av1);
    assert_eq!(packets.len(), 72, "one packet per picture");

    let delay = i64::from(enc.delay());
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(p.dts, k as i64 - delay, "dts of packet {k}");
        assert!(p.pts >= p.dts, "pts {} before dts {}", p.pts, p.dts);
    }
    assert!(packets.windows(2).all(|p| p[0].dts < p[1].dts), "dts strictly increases");
    let mut pts: Vec<i64> = packets.iter().map(|p| p.pts).collect();
    pts.sort_unstable();
    assert_eq!(pts, (0..72).collect::<Vec<_>>(), "every picture once");
    let keys: Vec<i64> = packets.iter().filter(|p| p.key).map(|p| p.pts).collect();
    assert_eq!(keys, vec![0, 24, 48], "AV1 keyframes every 24 pictures");
    eprintln!("AV1 B-frame delay {delay}");

    // Sized OBUs only; no OBU_TEMPORAL_DELIMITER (2) or OBU_PADDING (15).
    for (k, p) in packets.iter().enumerate() {
        let types = sample_obu_types(&p.data);
        assert!(!types.is_empty(), "packet {k} has no OBUs");
        assert!(types.iter().all(|&t| t != 2 && t != 7 && t != 15), "packet {k} has stripped OBU types {types:?}");
        if p.key {
            assert!(craft_video::av1::decode::is_key_frame_unit(&p.data), "keyframe {k}: types {types:?}");
        }
    }

    // Check the av1C record.
    let av1c = enc.av1_config().expect("an av1C record").clone();
    assert_eq!(Av1Config::parse(&av1c.to_bytes()).unwrap(), av1c);
    assert_eq!((av1c.seq_profile, av1c.high_bitdepth, av1c.twelve_bit, av1c.monochrome), (0, false, false, false));
    assert!((av1c.chroma_subsampling_x, av1c.chroma_subsampling_y) == (true, true));
    let obu_h = av1c.config_obus[0];
    assert_eq!((obu_h >> 3) & 0x0f, 1, "av1C config_obus starts with OBU_SEQUENCE_HEADER");
    let seq_payload = {
        let mut pos = if obu_h & 0x04 != 0 { 2 } else { 1 };
        while av1c.config_obus[pos] & 0x80 != 0 {
            pos += 1;
        }
        pos += 1;
        &av1c.config_obus[pos..]
    };
    let seq = SequenceHeader::parse(seq_payload).unwrap();
    assert_eq!((seq.max_frame_width, seq.max_frame_height), (w, h));
    eprintln!("av1C: profile {} level {} tier {}", av1c.seq_profile, av1c.seq_level_idx_0, av1c.seq_tier_0);

    // Decode with our own AV1 software decoder.
    let entry = SampleEntry::av1(av1c, w as u16, h as u16);
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
    eprintln!("AV1 worst luma PSNR {worst:.1} dB, worst chroma {worst_chroma:.1} dB, {} bytes", packets.iter().map(|p| p.data.len()).sum::<usize>());
    assert!(worst > 42.0, "luma PSNR {worst:.1} dB");
    assert!(worst_chroma > 44.0, "chroma PSNR {worst_chroma:.1} dB");
}

#[test]
fn an_encoder_dropped_mid_stream_releases_everything() {
    let _one = crate::common::NVENC_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !av1_available() {
        eprintln!("SKIPPED: no NVENC AV1 here");
        return;
    }
    for round in 0..8 {
        let mut enc = Nvenc::new(&config(640, 360)).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..5 {
            filmcraft_export::rgba_to_yuv420_8(&rgba(640, 360, i), 640, 360, &mut y, &mut u, &mut v);
            enc.encode(&y, &u, &v, i as u64).unwrap();
        }
    }
}

#[test]
fn a_keyframe_every_one_or_two_pictures_is_written_without_b_frames() {
    let _one = crate::common::NVENC_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for keyint in [1u32, 2] {
        let cfg = Config { keyint, ..config(640, 360) };
        let Some((enc, packets, _)) = encode_all(&cfg, 12) else { return };
        assert_eq!(enc.delay(), 0, "keyint {keyint}");
        assert_eq!(packets.len(), 12);
        assert!(packets.iter().enumerate().all(|(k, p)| p.pts == k as i64 && p.dts == p.pts), "keyint {keyint}: no reordering");
        let keys: Vec<i64> = packets.iter().filter(|p| p.key).map(|p| p.pts).collect();
        assert_eq!(keys, (0..12).step_by(keyint as usize).collect::<Vec<i64>>(), "keyint {keyint}");
        let entry = SampleEntry::av1(enc.av1_config().expect("an av1C record").clone(), 640, 360);
        let mut dec = filmcraft_codecs::software_video_decoder(&entry).unwrap();
        let mut n = 0;
        for p in &packets {
            n += dec.decode(&p.data, p.pts).unwrap().len();
        }
        assert_eq!(n + dec.flush().len(), 12, "keyint {keyint}");
    }
}
