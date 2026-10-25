//! External oracle verification against FFmpeg / libdav1d.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use filmcraft_av1enc::{Encoder, EncoderConfig, Preset, RateControl, YuvFrame};
use filmcraft_isobmff::{Av1Config, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

fn temp_mp4(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("filmcraft-av1enc-oracle");
    let _ = fs::create_dir_all(&dir);
    dir.join(name)
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
        let frame = YuvFrame {
            y: y_plane,
            u: uv_plane,
            v: uv_plane,
            y_stride: w as usize,
            uv_stride: (w / 2) as usize,
        };
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
    let mp4_path = temp_mp4("test_av1enc_ffmpeg_642x362.mp4");
    fs::write(&mp4_path, &mp4_bytes).unwrap();

    // 1. ffprobe stream inspection
    let probe_out = Command::new(&ffprobe)
        .args(["-v", "error", "-show_entries", "stream=codec_name,profile,width,height,level", "-of", "json"])
        .arg(&mp4_path)
        .output()
        .unwrap();
    assert!(probe_out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&probe_out.stderr));
    let probe_json = String::from_utf8_lossy(&probe_out.stdout);
    assert!(probe_json.contains("\"codec_name\": \"av1\""), "expected av1 codec, got {probe_json}");
    assert!(probe_json.contains("\"width\": 642"), "expected width 642, got {probe_json}");
    assert!(probe_json.contains("\"height\": 362"), "expected height 362, got {probe_json}");
    assert!(probe_json.contains("\"level\": 8"), "expected level 8 (Level 4.0), got {probe_json}");

    // 2. ffmpeg full stream decode
    let ffmpeg_out = Command::new(&ffmpeg)
        .args(["-v", "error", "-xerror", "-i"])
        .arg(&mp4_path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(ffmpeg_out.status.success(), "ffmpeg decode failed: {}", String::from_utf8_lossy(&ffmpeg_out.stderr));

    // 3. in-tree filmcraft-av1 decoder verification
    let mut dec = filmcraft_av1::Decoder::new();
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

    let _ = fs::remove_file(&mp4_path);
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
    let frame = YuvFrame {
        y: &y_plane,
        u: &uv_plane,
        v: &uv_plane,
        y_stride: w as usize,
        uv_stride: (w / 2) as usize,
    };
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
    let mp4_path = temp_mp4("test_av1enc_ffmpeg_4k_level12.mp4");
    fs::write(&mp4_path, &mp4_bytes).unwrap();

    // Verify ffprobe sees level 12 (Level 5.0) matching sequence header and av1C
    let probe_out = Command::new(&ffprobe)
        .args(["-v", "error", "-show_entries", "stream=codec_name,width,height,level", "-of", "json"])
        .arg(&mp4_path)
        .output()
        .unwrap();
    assert!(probe_out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&probe_out.stderr));
    let probe_json = String::from_utf8_lossy(&probe_out.stdout);
    assert!(probe_json.contains("\"codec_name\": \"av1\""), "expected av1, got {probe_json}");
    assert!(probe_json.contains("\"width\": 3840"), "expected width 3840, got {probe_json}");
    assert!(probe_json.contains("\"height\": 2160"), "expected height 2160, got {probe_json}");
    assert!(probe_json.contains("\"level\": 12"), "expected level 12 (Level 5.0), got {probe_json}");

    // Verify ffmpeg decodes 4K without errors
    let ffmpeg_out = Command::new(&ffmpeg)
        .args(["-v", "error", "-xerror", "-i"])
        .arg(&mp4_path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(ffmpeg_out.status.success(), "ffmpeg 4k decode failed: {}", String::from_utf8_lossy(&ffmpeg_out.stderr));

    let _ = fs::remove_file(&mp4_path);
}
