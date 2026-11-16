//! External oracle verification against FFmpeg / libdav1d.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use filmcraft_av1enc::{Encoder, EncoderConfig, Preset, RateControl, YuvFrame};
use filmcraft_isobmff::{Av1Config, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

struct TempMp4(PathBuf);

impl Drop for TempMp4 {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temp_mp4(name: &str) -> TempMp4 {
    let dir = std::env::temp_dir().join("filmcraft-av1enc-oracle");
    let _ = fs::create_dir_all(&dir);
    TempMp4(filmcraft_testkit::temp_path(&dir.join(name)))
}

#[test]
fn ffmpeg_decodes_intra_and_inter_residual_stream() {
    let ffmpeg = filmcraft_testkit::require_ffmpeg!();
    let ffprobe = filmcraft_testkit::require_ffprobe!();

    // Use non-block-aligned dimension (642x362) to stress boundary partitioning
    let (w, h) = (642u32, 362u32);
    let mut cfg = EncoderConfig::new(w, h, 24, 1);
    cfg.preset = Preset::Balanced;
    cfg.key_int = 30;
    cfg.rate_control = Some(RateControl::Qindex(90));

    let mut enc = Encoder::new(cfg).unwrap();
    assert_eq!(enc.seq_level_idx(), 8, "Expected level 4.0 (idx 8) for 642x362");

    let mut packets = Vec::new();
    let mut frames = Vec::new();

    // Generate 6 frames with natural gradient motion to trigger intra, inter, and DCT residuals
    for i in 0..6 {
        let mut y_plane = vec![128u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let grad = ((x.wrapping_add(i * 4) ^ y) & 0x7F) as u8;
                y_plane[(y * w + x) as usize] = 64u8.saturating_add(grad);
            }
        }
        let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
        frames.push((y_plane, uv_plane));
    }

    for (i, (y_plane, uv_plane)) in frames.iter().enumerate() {
        let frame = YuvFrame { y: y_plane, u: uv_plane, v: uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };
        let p = enc.encode(&frame, (i as i64) * 1000).unwrap();
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
    let mp4_file = temp_mp4("test_av1enc_ffmpeg_642x362.mp4");
    let mp4_path = &mp4_file.0;
    fs::write(mp4_path, &mp4_bytes).unwrap();

    // 1. ffprobe stream inspection
    let probe_out = Command::new(&ffprobe)
        .args(["-v", "error", "-show_entries", "stream=codec_name,profile,width,height,level", "-of", "json"])
        .arg(mp4_path)
        .output()
        .unwrap();
    assert!(probe_out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&probe_out.stderr));
    let probe_json = String::from_utf8_lossy(&probe_out.stdout);
    assert!(probe_json.contains("\"codec_name\": \"av1\""), "expected av1 codec, got {probe_json}");
    assert!(probe_json.contains("\"width\": 642"), "expected width 642, got {probe_json}");
    assert!(probe_json.contains("\"height\": 362"), "expected height 362, got {probe_json}");
    assert!(probe_json.contains("\"level\": 8"), "expected level 8 (Level 4.0), got {probe_json}");

    // 2. ffmpeg full stream decode (default decoder)
    let ffmpeg_out = Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-i"]).arg(mp4_path).args(["-f", "null", "-"]).output().unwrap();
    assert!(ffmpeg_out.status.success(), "ffmpeg decode failed: {}", String::from_utf8_lossy(&ffmpeg_out.stderr));

    // 2b. explicit libdav1d and libaom-av1 decode checks when available in ffmpeg build
    let decoders_out = Command::new(&ffmpeg).args(["-hide_banner", "-decoders"]).output().unwrap();
    let decoders_str = String::from_utf8_lossy(&decoders_out.stdout);
    if decoders_str.contains("libdav1d") {
        let dav1d_out =
            Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-c:v", "libdav1d", "-i"]).arg(mp4_path).args(["-f", "null", "-"]).output().unwrap();
        assert!(dav1d_out.status.success(), "libdav1d decode failed: {}", String::from_utf8_lossy(&dav1d_out.stderr));
    }
    if decoders_str.contains("libaom-av1") {
        let aom_out =
            Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-c:v", "libaom-av1", "-i"]).arg(mp4_path).args(["-f", "null", "-"]).output().unwrap();
        assert!(aom_out.status.success(), "libaom-av1 decode failed: {}", String::from_utf8_lossy(&aom_out.stderr));
    }

    // 3. in-tree craft-video AV1 decoder verification
    let mut dec = craft_video::av1::decode::Decoder::new();
    let mut decoded_pics = Vec::new();
    for p in &packets {
        decoded_pics.extend(dec.decode(&p.data).unwrap());
    }
    decoded_pics.extend(dec.flush());
    assert_eq!(decoded_pics.len(), 6, "All 6 frames must decode successfully in in-tree decoder");
    for pic in &decoded_pics {
        assert_eq!(pic.width, 642);
        assert_eq!(pic.height, 362);
    }

    verify_textured_moving_stream_quality(&ffmpeg, &decoders_str, w, h);
}

fn generate_textured_moving_frame(w: u32, h: u32, f_idx: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut y_p = vec![0u8; (w * h) as usize];
    let mut u_p = vec![0u8; ((w / 2) * (h / 2)) as usize];
    let mut v_p = vec![0u8; ((w / 2) * (h / 2)) as usize];
    let sq_x = (40 + f_idx * 28) % (w - 120);
    let sq_y = (30 + f_idx * 14) % (h - 120);
    for y in 0..h {
        for x in 0..w {
            let base = ((x * 180 / w) + (y * 60 / h)) as i32;
            let hash = (x.wrapping_mul(1103515245).wrapping_add(y.wrapping_mul(12345)).wrapping_add(f_idx.wrapping_mul(2654435761))) >> 16;
            let noise = ((hash & 0xF) as i32) - 8;
            let in_sq = x >= sq_x && x < sq_x + 120 && y >= sq_y && y < sq_y + 120;
            let val = if in_sq { 220 + noise } else { (base + noise).clamp(16, 235) };
            y_p[(y * w + x) as usize] = val as u8;
        }
    }
    for cy in 0..(h / 2) {
        for cx in 0..(w / 2) {
            let in_sq = (cx * 2) >= sq_x && (cx * 2) < sq_x + 120 && (cy * 2) >= sq_y && (cy * 2) < sq_y + 120;
            u_p[(cy * (w / 2) + cx) as usize] = if in_sq { 64 } else { (96 + ((cx + f_idx * 3) % 64)) as u8 };
            v_p[(cy * (w / 2) + cx) as usize] = if in_sq { 200 } else { (160 - ((cy + f_idx * 2) % 64)) as u8 };
        }
    }
    (y_p, u_p, v_p)
}

#[allow(clippy::too_many_arguments)]
fn verify_decoder_matches_in_tree_and_psnr(
    ffmpeg: &std::path::Path,
    dec_name: &str,
    tag: &str,
    mp4_path: &std::path::Path,
    mp4_len: usize,
    w: u32,
    h: u32,
    src_frames: &[(Vec<u8>, Vec<u8>, Vec<u8>)],
    in_tree_pics: &[craft_video::av1::decode::Picture],
) -> f64 {
    let y_len = (w * h) as usize;
    let uv_len = ((w / 2) * (h / 2)) as usize;
    let frame_bytes = y_len + 2 * uv_len;
    let out = Command::new(ffmpeg)
        .args(["-v", "error", "-xerror", "-c:v", dec_name, "-i"])
        .arg(mp4_path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{dec_name} failed on {tag}: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len(), src_frames.len() * frame_bytes, "{dec_name} raw output size mismatch on {tag}");

    let mut min_y_psnr = 100.0f64;
    let mut min_u_psnr = 100.0f64;
    let mut min_v_psnr = 100.0f64;
    for (fi, ((src_y, src_u, src_v), pic)) in src_frames.iter().zip(in_tree_pics.iter()).enumerate() {
        let base = fi * frame_bytes;
        let dec_y = &out.stdout[base..base + y_len];
        let dec_u = &out.stdout[base + y_len..base + y_len + uv_len];
        let dec_v = &out.stdout[base + y_len + uv_len..base + frame_bytes];

        for (idx, (&dy, &iy)) in dec_y.iter().zip(pic.planes[0].iter()).enumerate() {
            assert_eq!(dy as u16, iy, "{dec_name} vs in-tree Y mismatch at frame {fi} idx {idx}");
        }
        for (idx, (&du, &iu)) in dec_u.iter().zip(pic.planes[1].iter()).enumerate() {
            assert_eq!(du as u16, iu, "{dec_name} vs in-tree U mismatch at frame {fi} idx {idx}");
        }
        for (idx, (&dv, &iv)) in dec_v.iter().zip(pic.planes[2].iter()).enumerate() {
            assert_eq!(dv as u16, iv, "{dec_name} vs in-tree V mismatch at frame {fi} idx {idx}");
        }
        min_y_psnr = min_y_psnr.min(filmcraft_av1enc::compute_psnr(src_y, dec_y).unwrap());
        min_u_psnr = min_u_psnr.min(filmcraft_av1enc::compute_psnr(src_u, dec_u).unwrap());
        min_v_psnr = min_v_psnr.min(filmcraft_av1enc::compute_psnr(src_v, dec_v).unwrap());
    }
    println!("{tag} ({dec_name}, {mp4_len} bytes): min PSNR Y={min_y_psnr:.2} dB, U={min_u_psnr:.2} dB, V={min_v_psnr:.2} dB");
    assert!(min_y_psnr >= 26.0, "expected {tag} Y PSNR >= 26.0 dB, got {min_y_psnr:.2} dB");
    assert!(min_u_psnr >= 27.5, "expected {tag} U PSNR >= 27.5 dB, got {min_u_psnr:.2} dB");
    assert!(min_v_psnr >= 26.0, "expected {tag} V PSNR >= 26.0 dB, got {min_v_psnr:.2} dB");
    min_y_psnr
}

fn verify_textured_moving_stream_quality(ffmpeg: &std::path::Path, decoders_str: &str, w: u32, h: u32) {
    let mut q90_y_psnr = 0.0f64;
    let mut cq25_y_psnr = 0.0f64;
    let mut cq0_y_psnr = 0.0f64;
    for (tag, rc) in [("q90", RateControl::Qindex(90)), ("cq25", RateControl::ConstantQuality(25.0)), ("cq0", RateControl::ConstantQuality(0.0))] {
        let mut cfg = EncoderConfig::new(w, h, 24, 1);
        cfg.preset = Preset::Quality;
        cfg.key_int = 30;
        cfg.rate_control = Some(rc);
        let mut enc = Encoder::new(cfg).unwrap();
        let mut pkts = Vec::new();
        let mut src_frames = Vec::new();
        for f_idx in 0..12u32 {
            let (y_p, u_p, v_p) = generate_textured_moving_frame(w, h, f_idx);
            let yf = YuvFrame { y: &y_p, u: &u_p, v: &v_p, y_stride: w as usize, uv_stride: (w / 2) as usize };
            pkts.push(enc.encode(&yf, (f_idx as i64) * 1000).unwrap());
            src_frames.push((y_p, u_p, v_p));
        }
        let mut cur = Cursor::new(Vec::new());
        let mut wr = Mp4Writer::new(&mut cur, WriterOptions::new(Brand::Mp4)).unwrap();
        let tid = wr
            .add_track(TrackConfig::new(
                SampleEntry::av1(
                    Av1Config {
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
                    },
                    w as u16,
                    h as u16,
                ),
                24,
            ))
            .unwrap();
        for p in &pkts {
            wr.write_sample(tid, WriteSample { data: &p.data, duration: 1, is_sync: p.keyframe, composition_offset: 0 }).unwrap();
        }
        wr.finish().unwrap();
        let mp4_data = cur.into_inner();
        let f_mp4 = temp_mp4(&format!("test_av1enc_textured_{tag}.mp4"));
        fs::write(&f_mp4.0, &mp4_data).unwrap();

        let mut in_tree_dec = craft_video::av1::decode::Decoder::new();
        let mut in_tree_pics = Vec::new();
        for p in &pkts {
            in_tree_pics.extend(in_tree_dec.decode(&p.data).unwrap());
        }
        in_tree_pics.extend(in_tree_dec.flush());
        assert_eq!(in_tree_pics.len(), 12);

        for dec_name in ["libaom-av1", "libdav1d"] {
            if decoders_str.contains(dec_name) {
                let min_y_psnr = verify_decoder_matches_in_tree_and_psnr(ffmpeg, dec_name, tag, &f_mp4.0, mp4_data.len(), w, h, &src_frames, &in_tree_pics);
                match tag {
                    "q90" => q90_y_psnr = min_y_psnr,
                    "cq25" => cq25_y_psnr = min_y_psnr,
                    "cq0" => cq0_y_psnr = min_y_psnr,
                    _ => {}
                }
            }
        }
    }
    assert!(
        cq0_y_psnr > q90_y_psnr + 3.0 && cq0_y_psnr > cq25_y_psnr + 3.0,
        "expected ConstantQuality(0.0) Y PSNR ({cq0_y_psnr:.2} dB) to exceed Qindex(90) ({q90_y_psnr:.2} dB) and ConstantQuality(25.0) ({cq25_y_psnr:.2} dB) by > 3.0 dB"
    );
}

#[test]
fn ffmpeg_and_ffprobe_verify_4k_level_12_agreement() {
    let ffmpeg = filmcraft_testkit::require_ffmpeg!();
    let ffprobe = filmcraft_testkit::require_ffprobe!();

    // 4K UHD resolution (3840x2160)
    let (w, h) = (3840u32, 2160u32);
    let mut cfg = EncoderConfig::new(w, h, 24, 1);
    cfg.preset = Preset::Speed;
    cfg.rate_control = Some(RateControl::Qindex(120));

    let mut enc = Encoder::new(cfg).unwrap();
    assert_eq!(enc.seq_level_idx(), 12, "Expected level 5.0 (idx 12) for 4K");

    let y_plane = vec![128u8; (w * h) as usize];
    let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
    let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };
    let packet = enc.encode(&frame, 0).unwrap();

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
    writer.write_sample(track_id, WriteSample { data: &packet.data, duration: 1, is_sync: packet.keyframe, composition_offset: 0 }).unwrap();
    writer.finish().unwrap();

    let mp4_bytes = cursor.into_inner();
    let mp4_file = temp_mp4("test_av1enc_ffmpeg_4k_level12.mp4");
    let mp4_path = &mp4_file.0;
    fs::write(mp4_path, &mp4_bytes).unwrap();

    // Verify ffprobe sees level 12 (Level 5.0) matching sequence header and av1C
    let probe_out =
        Command::new(&ffprobe).args(["-v", "error", "-show_entries", "stream=codec_name,width,height,level", "-of", "json"]).arg(mp4_path).output().unwrap();
    assert!(probe_out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&probe_out.stderr));
    let probe_json = String::from_utf8_lossy(&probe_out.stdout);
    assert!(probe_json.contains("\"codec_name\": \"av1\""), "expected av1, got {probe_json}");
    assert!(probe_json.contains("\"width\": 3840"), "expected width 3840, got {probe_json}");
    assert!(probe_json.contains("\"height\": 2160"), "expected height 2160, got {probe_json}");
    assert!(probe_json.contains("\"level\": 12"), "expected level 12 (Level 5.0), got {probe_json}");

    // Verify ffmpeg decodes 4K without errors (default decoder + explicit libdav1d and libaom-av1)
    let ffmpeg_out = Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-i"]).arg(mp4_path).args(["-f", "null", "-"]).output().unwrap();
    assert!(ffmpeg_out.status.success(), "ffmpeg 4k decode failed: {}", String::from_utf8_lossy(&ffmpeg_out.stderr));

    let decoders_out = Command::new(&ffmpeg).args(["-hide_banner", "-decoders"]).output().unwrap();
    let decoders_str = String::from_utf8_lossy(&decoders_out.stdout);
    for dec_name in ["libdav1d", "libaom-av1"] {
        if decoders_str.contains(dec_name) {
            let dec_out =
                Command::new(&ffmpeg).args(["-v", "error", "-xerror", "-c:v", dec_name, "-i"]).arg(mp4_path).args(["-f", "null", "-"]).output().unwrap();
            assert!(dec_out.status.success(), "{dec_name} 4k decode failed: {}", String::from_utf8_lossy(&dec_out.stderr));
        }
    }
}
