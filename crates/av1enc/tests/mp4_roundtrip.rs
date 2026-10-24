//! Test packaging encoded AV1 frames into an ISOBMFF / MP4 container.

use std::io::Cursor;

use filmcraft_av1enc::{Encoder, EncoderConfig, YuvFrame};
use filmcraft_isobmff::{Av1Config, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

#[test]
fn test_av1enc_mp4_mux_and_decode() {
    let (w, h) = (320u32, 240u32);
    let mut enc = Encoder::new(EncoderConfig::new(w, h, 24, 1)).unwrap();
    let y_plane = vec![128u8; (w * h) as usize];
    let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
    let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

    let mut packets = Vec::new();
    for i in 0..5 {
        let p = enc.encode(&frame, i * 1000).unwrap();
        packets.push(p);
    }

    let av1c = Av1Config {
        seq_profile: 0,
        seq_level_idx_0: enc.seq_level_idx(),
        seq_tier_0: false,
        high_bitdepth: false,
        twelve_bit: false,
        monochrome: false,
        chroma_subsampling_x: true,
        chroma_subsampling_y: true,
        chroma_sample_position: 0,
        initial_presentation_delay_minus_one: None,
        config_obus: enc.sequence_header_obu(),
    };

    let sample_entry = SampleEntry::av1(av1c, w as u16, h as u16);
    let track_cfg = TrackConfig::new(sample_entry, 24);

    let mut cursor = Cursor::new(Vec::new());
    let mut writer = Mp4Writer::new(&mut cursor, WriterOptions::new(Brand::Mp4)).unwrap();
    let track_id = writer.add_track(track_cfg).unwrap();

    for p in &packets {
        writer.write_sample(track_id, WriteSample { data: &p.data, duration: 1, is_sync: p.keyframe, composition_offset: 0 }).unwrap();
    }
    writer.finish().unwrap();

    let mp4_bytes = cursor.into_inner();
    assert!(!mp4_bytes.is_empty(), "MP4 bytes must not be empty");

    // Open MP4 and verify demux
    let mp4 = filmcraft_isobmff::open(&mp4_bytes).unwrap();
    let tracks = &mp4.tracks;
    assert_eq!(tracks.len(), 1);
    let trk = &tracks[0];
    assert_eq!(trk.samples.len(), 5);

    // Decode all frames with AV1 decoder
    let mut dec = filmcraft_av1::Decoder::new();
    let mut decoded_pics = Vec::new();
    for s in &trk.samples {
        let sample_data = &mp4_bytes[s.offset as usize..s.offset as usize + s.size as usize];
        decoded_pics.extend(dec.decode(sample_data).unwrap());
    }
    decoded_pics.extend(dec.flush());

    assert_eq!(decoded_pics.len(), 5, "Decoded all 5 frames from MP4");
    assert_eq!(decoded_pics[0].width, w);
    assert_eq!(decoded_pics[0].height, h);
    assert_eq!(decoded_pics[0].planes[0][0], 128);

    let psnr = filmcraft_av1enc::compute_psnr_u16(&y_plane, &decoded_pics[0].planes[0]).unwrap();
    assert_eq!(psnr, 100.0, "Identical flat plane must have infinite/100dB PSNR");
}

#[test]
fn test_av1enc_mp4_mux_and_decode_gradient_psnr() {
    let (w, h) = (128u32, 128u32);
    let mut enc = Encoder::new(EncoderConfig::new(w, h, 30, 1)).unwrap();
    let mut y_plane = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            y_plane[(y * w + x) as usize] = (y * 2).min(255) as u8;
        }
    }
    let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
    let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

    let p = enc.encode(&frame, 0).unwrap();
    assert!(!p.data.is_empty());

    let mut dec = filmcraft_av1::Decoder::new();
    let mut pics = dec.decode(&p.data).unwrap();
    pics.extend(dec.flush());
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].width, w);
    assert_eq!(pics[0].height, h);

    let psnr = filmcraft_av1enc::compute_psnr_u16(&y_plane, &pics[0].planes[0]).unwrap();
    println!("psnr: {psnr:.2} dB");
    for by in 0..4 {
        let mut line = String::new();
        for bx in 0..4 {
            let val = pics[0].planes[0][(by * 32 + 16) * w as usize + (bx * 32 + 16)];
            line.push_str(&format!("{val:4} "));
        }
        println!("grid by={by}: {line}");
    }
    // In intra mode with transform residuals and directional prediction, reconstructed PSNR improves.
    assert!(psnr > 10.0, "Reconstruction PSNR should be > 10 dB, got {psnr:.2} dB");
}

#[test]
fn test_av1enc_inter_p_frames_roundtrip() {
    let (w, h) = (320u32, 240u32);
    // key_int = 10, so frame 0 is KeyFrame, frames 1..5 are InterFrames
    let mut enc = Encoder::new(EncoderConfig::new(w, h, 24, 10)).unwrap();
    let mut y_plane = vec![100u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            y_plane[(y * w + x) as usize] = ((x + y) % 256) as u8;
        }
    }
    let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
    let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

    let mut packets = Vec::new();
    for i in 0..6 {
        let p = enc.encode(&frame, i * 1000).unwrap();
        packets.push(p);
    }

    assert!(packets[0].keyframe, "Frame 0 must be a keyframe");
    println!("Frame 0 (KeyFrame) size = {} bytes", packets[0].data.len());
    for (i, p) in packets.iter().enumerate().skip(1) {
        assert!(!p.keyframe, "Frame {i} must be an inter (P) frame");
        println!("Frame {i} (Inter P-frame) size = {} bytes", p.data.len());
        // Static scene inter frames should be much smaller than keyframe
        assert!(p.data.len() < packets[0].data.len(), "Inter frame {i} size ({}) must be smaller than keyframe size ({})", p.data.len(), packets[0].data.len());
    }

    // Decode all frames with AV1 decoder
    let mut dec = filmcraft_av1::Decoder::new();
    let mut decoded = Vec::new();
    for p in &packets {
        decoded.extend(dec.decode(&p.data).unwrap());
    }
    decoded.extend(dec.flush());

    assert_eq!(decoded.len(), 6, "All 6 frames (1 Key + 5 Inter) must decode successfully");
    for (i, pic) in decoded.iter().enumerate() {
        assert_eq!(pic.width, w);
        assert_eq!(pic.height, h);
        let psnr = filmcraft_av1enc::compute_psnr_u16(&y_plane, &pic.planes[0]).unwrap();
        println!("Frame {i} decoded PSNR = {psnr:.2} dB");
        assert!(psnr > 9.0, "Frame {i} decoded PSNR must be > 9 dB, got {psnr:.2} dB");
    }
}

#[test]
fn test_av1enc_scene_cut_detection() {
    let (w, h) = (128u32, 128u32);
    // key_int is 50, so normally no keyframe until frame 50
    let mut enc = Encoder::new(EncoderConfig::new(w, h, 24, 50)).unwrap();

    let scene_a_y = vec![40u8; (w * h) as usize];
    let scene_b_y = vec![220u8; (w * h) as usize];
    let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];

    let frame_a = YuvFrame { y: &scene_a_y, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };
    let frame_b = YuvFrame { y: &scene_b_y, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

    let p0 = enc.encode(&frame_a, 0).unwrap();
    let p1 = enc.encode(&frame_a, 1000).unwrap();
    let p2 = enc.encode(&frame_a, 2000).unwrap();
    // Scene cut to Scene B at frame 3!
    let p3 = enc.encode(&frame_b, 3000).unwrap();
    let p4 = enc.encode(&frame_b, 4000).unwrap();

    assert!(p0.keyframe, "Frame 0 must be keyframe");
    assert!(!p1.keyframe, "Frame 1 must be P-frame");
    assert!(!p2.keyframe, "Frame 2 must be P-frame");
    assert!(p3.keyframe, "Frame 3 must be detected as SCENE CUT and encoded as KEYFRAME!");
    assert!(!p4.keyframe, "Frame 4 must be P-frame referencing Scene B keyframe");

    // All frames decode successfully
    let mut dec = filmcraft_av1::Decoder::new();
    let mut pics = Vec::new();
    for p in [&p0, &p1, &p2, &p3, &p4] {
        pics.extend(dec.decode(&p.data).unwrap());
    }
    pics.extend(dec.flush());

    assert_eq!(pics.len(), 5);
    assert_eq!(pics[0].planes[0][0], 40);
    assert_eq!(pics[3].planes[0][0], 220);
    assert_eq!(pics[4].planes[0][0], 220);
}

#[test]
fn test_av1enc_non_64_colored_matte_160x96() {
    use filmcraft_av1enc::Preset;

    // 160x96 blue matte (Y=82, U=170, V=90) — reproduces the exact geometry and chroma
    // residuals from Chaython/filmcraft#1 where 160 = 2*64 + 32 and 96 = 1*64 + 32.
    for preset in [Preset::Speed, Preset::Balanced, Preset::Quality] {
        let (w, h) = (160u32, 96u32);
        let mut cfg = EncoderConfig::new(w, h, 24, 10);
        cfg.preset = preset;
        let mut enc = Encoder::new(cfg).unwrap();

        let y_plane = vec![82u8; (w * h) as usize];
        let u_plane = vec![170u8; ((w / 2) * (h / 2)) as usize];
        let v_plane = vec![90u8; ((w / 2) * (h / 2)) as usize];
        let frame = YuvFrame { y: &y_plane, u: &u_plane, v: &v_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

        let mut dec = filmcraft_av1::Decoder::new();
        let mut decoded = Vec::new();
        for i in 0..4 {
            let pkt = enc.encode(&frame, i * 1000).unwrap();
            decoded.extend(dec.decode(&pkt.data).unwrap());
        }
        decoded.extend(dec.flush());

        assert_eq!(decoded.len(), 4);
        for pic in &decoded {
            assert_eq!(pic.width, w);
            assert_eq!(pic.height, h);
            let psnr_y = filmcraft_av1enc::compute_psnr_u16(&y_plane, &pic.planes[0]).unwrap();
            let psnr_u = filmcraft_av1enc::compute_psnr_u16(&u_plane, &pic.planes[1]).unwrap();
            let psnr_v = filmcraft_av1enc::compute_psnr_u16(&v_plane, &pic.planes[2]).unwrap();
            println!(
                "preset {preset:?}: psnr_y={psnr_y:.2} psnr_u={psnr_u:.2} psnr_v={psnr_v:.2} | Y[0]={} Y[70]={} Y[140]={} Y[64*160]={}",
                pic.planes[0][0],
                pic.planes[0][70],
                pic.planes[0][140],
                pic.planes[0][64 * 160]
            );
            assert!(psnr_y > 35.0, "preset {preset:?} Y PSNR too low: {psnr_y}");
            assert!(psnr_u > 30.0, "preset {preset:?} U PSNR too low: {psnr_u}");
            assert!(psnr_v > 30.0, "preset {preset:?} V PSNR too low: {psnr_v}");
        }
    }
}

#[test]
fn test_av1enc_arbitrary_resolutions_colored_gradients() {
    use filmcraft_av1enc::Preset;

    // Resolutions exercising 64x64, 32x32, 16x16, and 8x8 boundary splits
    // with non-flat Y, U, and V planes across multiple superblock rows.
    for (w, h) in [(100u32, 60u32), (160, 96), (320, 240), (200, 136)] {
        let mut cfg = EncoderConfig::new(w, h, 30, 5);
        cfg.preset = Preset::Quality;
        let mut enc = Encoder::new(cfg).unwrap();

        let mut y_plane = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                y_plane[(y * w + x) as usize] = (40 + ((x * 160) / w) + ((y * 40) / h)).min(235) as u8;
            }
        }
        let uv_w = w / 2;
        let uv_h = h / 2;
        let mut u_plane = vec![0u8; (uv_w * uv_h) as usize];
        let mut v_plane = vec![0u8; (uv_w * uv_h) as usize];
        for y in 0..uv_h {
            for x in 0..uv_w {
                u_plane[(y * uv_w + x) as usize] = (60 + ((x * 140) / uv_w)).min(240) as u8;
                v_plane[(y * uv_w + x) as usize] = (200usize.saturating_sub(((y * 120) / uv_h) as usize)).max(16) as u8;
            }
        }

        let frame = YuvFrame { y: &y_plane, u: &u_plane, v: &v_plane, y_stride: w as usize, uv_stride: uv_w as usize };
        let mut dec = filmcraft_av1::Decoder::new();
        let mut pics = Vec::new();
        for i in 0..3 {
            let pkt = enc.encode(&frame, i * 1000).unwrap();
            pics.extend(dec.decode(&pkt.data).unwrap());
        }
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 3, "Failed for {w}x{h}");
        for pic in &pics {
            assert_eq!(pic.width, w);
            assert_eq!(pic.height, h);
        }
    }
}
