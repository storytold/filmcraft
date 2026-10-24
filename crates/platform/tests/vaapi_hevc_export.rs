//! HEVC Main through the real VA-API encoder and MP4 muxer; no mock hardware success.
#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

use filmcraft_export::{BitrateMode, EncoderFrame, ExportSettings, Format, HardwareEncoding};
use filmcraft_platform::vaapi::hevcenc;
use filmcraft_time::FrameRate;

#[test]
fn hevc_factory_never_changes_the_requested_codec() {
    let settings = ExportSettings { format: Format::H264, ..Default::default() };
    assert!(hevcenc::factory(Format::H264, 256, 256, FrameRate::FPS_30, &settings).is_none());
}

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hardware_case(width: u32, height: u32, frames: u64, keyint: u32, name: &str, mode: BitrateMode) {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if filmcraft_platform::vaapi::va::hevc_encode_caps().is_err() {
        eprintln!("SKIPPED: no functional VA-API HEVC Main encoder");
        return;
    }
    let settings = ExportSettings {
        format: Format::Hevc,
        hardware_encoding: HardwareEncoding::Off,
        bitrate_mode: mode,
        bitrate_kbps: 500,
        keyframe_distance: Some(keyint),
        ..Default::default()
    };
    let mut encoder = match hevcenc::factory(Format::Hevc, width, height, FrameRate::FPS_30, &settings) {
        Some(Ok(encoder)) => encoder,
        Some(Err(e)) => {
            eprintln!("SKIPPED: VA-API HEVC encoder did not start: {e:?}");
            return;
        }
        None => {
            eprintln!("SKIPPED: no VA-API HEVC encoder for this format");
            return;
        }
    };
    assert_eq!(encoder.timescale(), 30);
    let mut expected_yuv = Vec::new();
    let mut packets = Vec::new();
    for i in 0..frames {
        let colors = [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]];
        // Independent BT.709 limited-range values, rounded to 8-bit codes.
        let codes = [[63u8, 102, 240], [173, 42, 26], [32, 240, 118], [235, 128, 128]];
        let color_at = |x: usize, y: usize| ((usize::from(x >= width as usize / 2) + usize::from(y >= height as usize / 2) * 2) as u64 + i) as usize % 4;
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height as usize {
            for x in 0..width as usize {
                rgba.extend_from_slice(&colors[color_at(x, y)]);
            }
        }
        for plane in 0..3 {
            let scale = if plane == 0 { 1 } else { 2 };
            for y in 0..height as usize / scale {
                for x in 0..width as usize / scale {
                    let mut total = 0u32;
                    for dy in 0..scale {
                        for dx in 0..scale {
                            total += u32::from(codes[color_at(x * scale + dx, y * scale + dy)][plane]);
                        }
                    }
                    expected_yuv.push((total / (scale * scale) as u32) as u8);
                }
            }
        }
        let frame = EncoderFrame { rgba: &rgba, width, height, hdr: None, index: i };
        let mut encoded = encoder.encode(&frame).expect("real HEVC picture");
        assert_eq!(encoded.len(), 1);
        let packet = encoded.remove(0);
        assert_eq!(packet.key, i % u64::from(keyint) == 0);
        assert_eq!((packet.duration, packet.composition_offset), (1, 0));
        assert!(!packet.data.is_empty());
        packets.push(packet);
    }
    let entry = encoder.sample_entry();
    let filmcraft_isobmff::CodecConfig::Hevc(config) = &entry.codec else {
        panic!("not HEVC");
    };
    assert_eq!((config.general_profile_idc, config.bit_depth_luma, config.bit_depth_chroma), (1, 8, 8));
    assert_eq!(config.length_size, 4);
    assert!(!config.vps().is_empty() && !config.sps().is_empty() && !config.pps().is_empty());
    assert!(encoder.flush().unwrap().is_empty());
    assert_eq!(packets.len(), frames as usize);
    use filmcraft_isobmff::{Brand, Mp4Writer, TrackConfig, WriteSample, WriterOptions};
    let dir = filmcraft_testkit::fixtures_dir("vaapi-hevc");
    std::fs::create_dir_all(&dir).unwrap();
    for (brand, ext) in [(Brand::Mp4, "mp4"), (Brand::Mov, "mov")] {
        let path = dir.join(format!("{name}.{ext}"));
        let mut mux = Mp4Writer::new(std::fs::File::create(&path).unwrap(), WriterOptions::new(brand)).unwrap();
        let track = mux.add_track(TrackConfig::new(entry.clone(), encoder.timescale())).unwrap();
        for packet in &packets {
            mux.write_sample(
                track,
                WriteSample { data: &packet.data, duration: packet.duration, composition_offset: packet.composition_offset, is_sync: packet.key },
            )
            .unwrap();
        }
        mux.finish().unwrap();
        eprintln!("HEVC hardware artifact: {}", path.display());
        if let Some(probe) = filmcraft_testkit::ffprobe() {
            let result = std::process::Command::new(&probe)
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-count_frames",
                    "-show_entries",
                    "stream=codec_name,profile,codec_tag_string,pix_fmt,width,height,nb_read_frames",
                    "-of",
                    "default=noprint_wrappers=1",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(result.status.success(), "ffprobe: {}", String::from_utf8_lossy(&result.stderr));
            let info = String::from_utf8_lossy(&result.stdout);
            for expected in [
                "codec_name=hevc".to_string(),
                "profile=Main".into(),
                "codec_tag_string=hvc1".into(),
                "pix_fmt=yuv420p".into(),
                format!("width={width}"),
                format!("height={height}"),
                format!("nb_read_frames={frames}"),
            ] {
                assert!(info.lines().any(|line| line == expected), "{expected}: {info}");
            }
            let result = std::process::Command::new(probe)
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts,dts,duration,flags", "-of", "csv=p=0"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(result.status.success());
            let timing = String::from_utf8_lossy(&result.stdout);
            let lines: Vec<_> = timing.lines().collect();
            assert_eq!(lines.len(), frames as usize);
            for (i, line) in lines.iter().enumerate() {
                let fields: Vec<_> = line.split(',').collect();
                assert_eq!(fields[..3], [i.to_string(), i.to_string(), "1".into()]);
                assert_eq!(fields[3].contains('K'), i % keyint as usize == 0);
            }
        } else {
            eprintln!("SKIPPED: HEVC ffprobe oracle unavailable");
        }
        if let Some(ffmpeg) = filmcraft_testkit::ffmpeg() {
            let result = std::process::Command::new(ffmpeg)
                .args(["-v", "error", "-xerror", "-i"])
                .arg(&path)
                .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
                .output()
                .unwrap();
            assert!(result.status.success() && result.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&result.stderr));
            assert_eq!(result.stdout.len(), expected_yuv.len());
            let mse = result.stdout.iter().zip(&expected_yuv).map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2)).sum::<f64>() / expected_yuv.len() as f64;
            let psnr = if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64.powi(2) / mse).log10() };
            assert!(psnr > 30.0, "HEVC pattern colors/geometry differ: {psnr:.2} dB");
            eprintln!("HEVC Main {ext}: {frames} frames decoded, YUV PSNR {psnr:.2} dB");
        } else {
            eprintln!("SKIPPED: HEVC ffmpeg decode oracle unavailable");
        }
    }
}

#[test]
fn hevc_main_hardware_has_valid_configuration_and_ordered_packets() {
    hardware_case(256, 256, 7, 3, "main-pattern", BitrateMode::Cbr);
}
#[test]
fn hevc_1080p_crops_padding_and_accepts_vbr() {
    hardware_case(1920, 1080, 7, 3, "main-1080p", BitrateMode::Vbr1Pass);
}
#[test]
fn hevc_arbitrary_even_crop_keeps_geometry() {
    hardware_case(642, 362, 7, 3, "main-cropped", BitrateMode::Cbr);
}
#[test]
fn hevc_long_gop_wraps_poc_without_losing_frames() {
    hardware_case(256, 256, 301, 300, "main-long-gop", BitrateMode::Cbr);
}

#[test]
fn zero_timeout_probe_wait_returns_immediately() {
    let start = std::time::Instant::now();
    filmcraft_platform::wait_encoder_probes(std::time::Duration::ZERO);
    assert!(start.elapsed() < std::time::Duration::from_millis(50));
}
