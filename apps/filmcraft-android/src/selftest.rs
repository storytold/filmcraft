#![cfg(target_os = "android")]

//! Hardware-decoding self test: decode the embedded H.264, HEVC and HEVC Main 10 clips through
//! whatever decoders the registered factories produce (the MediaCodec backend from `crates/platform`
//! on Android) and report each decoder, its picture count and the hardware counters. The 10-bit
//! clip is additionally decoded with our own decoder and compared picture by picture, which checks
//! the 16-bit (P010) output conversion, not just that frames came out.

use filmcraft_codecs::DecodedFrame;
use filmcraft_codecs::hw;
use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_isobmff::{AvcConfig, CodecConfig, FourCc, HevcConfig, HevcNalArray, SampleEntry, VideoParams};

/// ffmpeg-generated clips: 320x240, 25-30 fps, 1 s, Annex-B (the last one HEVC Main 10, i.e.
/// 10-bit 4:2:0).
const CLIP_H264: &[u8] = include_bytes!("../assets/test.h264");
const CLIP_HEVC: &[u8] = include_bytes!("../assets/test.hevc");
const CLIP_HEVC10: &[u8] = include_bytes!("../assets/test-10bit.hevc");

/// NAL units as (start, end, type) with the start code skipped. HEVC packs the type in the high
/// bits of the first byte of a two-byte header, H.264 in the low five bits of one byte.
fn split_nals(stream: &[u8], hevc: bool) -> Vec<(usize, usize, u8)> {
    let mut starts: Vec<usize> = Vec::new();
    let n = stream.len();
    let mut i = 0;
    while i + 3 < n {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else if i + 4 < n && stream[i..i + 4] == [0, 0, 0, 1] {
            starts.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (idx, &start) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).map_or(n, |&s| s.saturating_sub(3)).min(n);
        if start < end {
            let byte = stream[start];
            let nal_type = if hevc { (byte >> 1) & 0x3f } else { byte & 0x1f };
            nals.push((start, end, nal_type));
        }
    }
    nals
}

/// VCL NAL types: H.264 1/5, HEVC 0–31.
fn is_vcl(nal_type: u8, hevc: bool) -> bool {
    if hevc { nal_type <= 31 } else { nal_type == 1 || nal_type == 5 }
}

/// Access units as length-prefixed (4-byte) samples, the way the engine feeds decoders.
fn access_units(stream: &[u8], hevc: bool) -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut seen_vcl = false;
    for (start, end, nal_type) in split_nals(stream, hevc) {
        let vcl = is_vcl(nal_type, hevc);
        if vcl && seen_vcl {
            units.push(std::mem::take(&mut current));
            seen_vcl = false;
        }
        let len = (end - start) as u32;
        current.extend_from_slice(&len.to_be_bytes());
        current.extend_from_slice(&stream[start..end]);
        if vcl {
            seen_vcl = true;
        }
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

/// A `320x240` visual sample entry for the given configuration.
fn entry_for(codec: CodecConfig) -> SampleEntry {
    SampleEntry {
        format: FourCc::new(b"avc1"),
        data_reference_index: 1,
        codec,
        video: Some(VideoParams { width: 320, height: 240, ..Default::default() }),
        audio: None,
        bitrate: None,
    }
}

/// Decode `clip` with the first decoder the registered factories offer and describe what happened.
///
/// `bits` is the stream's bit depth: 8, or 10 for the Main 10 clip (the sample entry carries it, and
/// a hardware decoder that takes the stream has to convert its 16-bit output back to 10-bit codes).
fn run_one(label: &str, clip: &[u8], hevc: bool, bits: u8) -> String {
    let nals = split_nals(clip, hevc);
    let find = |t: u8| nals.iter().find(|n| n.2 == t).map(|n| clip[n.0..n.1].to_vec());
    let entry = if hevc {
        let (Some(vps), Some(sps), Some(pps)) = (find(32), find(33), find(34)) else {
            return format!("{label} 自检: 测试流缺少 VPS/SPS/PPS");
        };
        let array = |nal_type: u8, nalu: Vec<u8>| HevcNalArray { completeness: true, nal_type, nalus: vec![nalu] };
        entry_for(CodecConfig::Hevc(HevcConfig {
            general_profile_idc: if bits > 8 { 2 } else { 1 },
            general_level_idc: 120,
            chroma_format_idc: 1,
            bit_depth_luma: bits,
            bit_depth_chroma: bits,
            length_size: 4,
            arrays: vec![array(32, vps), array(33, sps), array(34, pps)],
            ..Default::default()
        }))
    } else {
        let (Some(sps), Some(pps)) = (find(7), find(8)) else {
            return format!("{label} 自检: 测试流缺少 SPS/PPS");
        };
        entry_for(CodecConfig::Avc(AvcConfig {
            profile: sps.get(1).copied().unwrap_or(0x42),
            compatibility: sps.get(2).copied().unwrap_or(0),
            level: sps.get(3).copied().unwrap_or(0x1e),
            length_size: 4,
            sps: vec![sps],
            pps: vec![pps],
            ext: Vec::new(),
        }))
    };

    let before = hw::hw_stats();
    let mut decoder = match filmcraft_codecs::make_video_decoder(&entry) {
        Ok(d) => d,
        Err(e) => return format!("{label} 自检: 创建解码器失败: {e}"),
    };
    let name = decoder.name().to_string();
    let units = access_units(clip, hevc);
    let mut decoded: Vec<DecodedFrame> = Vec::new();
    let mut error = None;
    for (i, unit) in units.iter().enumerate() {
        match decoder.decode(unit, i as i64) {
            Ok(out) => decoded.extend(out),
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        }
    }
    decoded.extend(decoder.flush());
    let after = hw::hw_stats();
    let frames = decoded.len();
    let size = decoded.first().map(|f| format!("{}x{}", f.frame.width, f.frame.height)).unwrap_or_default();

    let mut report = format!(
        "{label} 自检: 解码器 {name} | 样本 {} 个 → 解出 {frames} 帧{} | 硬件计数 帧 {} 会话 {} 拒绝 {} 回退 {}",
        units.len(),
        if size.is_empty() { String::new() } else { format!("（{size}）") },
        after.frames - before.frames,
        after.sessions - before.sessions,
        after.declined - before.declined,
        after.fallbacks - before.fallbacks,
    );
    if let Some(e) = error {
        report.push_str(&format!(" | 错误: {e}"));
    }
    // 10-bit pictures the hardware decoded: our own decoder is the reference for the conversion the
    // backend makes (16-bit P010 samples → `PixelData::Yuv16` with `bits = 10`), so compare them
    // picture by picture. Skipped when the hardware declined the stream (the frames are the
    // reference itself then).
    if bits > 8 && after.sessions > before.sessions {
        report.push_str(&format!(" | 与软解对比: {}", compare_with_software(&entry, &units, &decoded)));
    }
    report
}

/// Decode the same access units with our own (software) decoder and say whether it produced the
/// same pictures: same count, same pts order, same size, layout and samples.
fn compare_with_software(entry: &SampleEntry, units: &[Vec<u8>], decoded: &[DecodedFrame]) -> String {
    let mut software = match filmcraft_codecs::software_video_decoder(entry) {
        Ok(d) => d,
        Err(e) => return format!("软解不可用: {e}"),
    };
    let mut reference: Vec<DecodedFrame> = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        match software.decode(unit, i as i64) {
            Ok(out) => reference.extend(out),
            Err(e) => return format!("软解失败: {e}"),
        }
    }
    reference.extend(software.flush());
    if reference.len() != decoded.len() {
        return format!("帧数不同（硬解 {} / 软解 {}）", decoded.len(), reference.len());
    }
    for (a, b) in decoded.iter().zip(reference.iter()) {
        if a.pts != b.pts {
            return format!("pts 顺序不同（硬解 {} / 软解 {}）", a.pts, b.pts);
        }
        if (a.frame.width, a.frame.height) != (b.frame.width, b.frame.height) {
            return format!("尺寸不同（pts {}：硬解 {}x{} / 软解 {}x{}）", a.pts, a.frame.width, a.frame.height, b.frame.width, b.frame.height);
        }
        if !same_pixels(&a.frame, &b.frame) {
            return format!("像素不同（pts {}：{}）", a.pts, first_difference(&a.frame, &b.frame));
        }
    }
    format!("{} 帧与软解逐像素一致", decoded.len())
}

/// Whether two frames hold the same picture: size, layout and every sample.
fn same_pixels(a: &VideoFrame, b: &VideoFrame) -> bool {
    if (a.width, a.height) != (b.width, b.height) {
        return false;
    }
    match (&a.data, &b.data) {
        (PixelData::Yuv8 { planes: p, chroma: c, .. }, PixelData::Yuv8 { planes: q, chroma: d, .. }) => c == d && p.iter().zip(q).all(|(x, y)| x == y),
        (PixelData::Yuv16 { planes: p, chroma: c, bits: n, .. }, PixelData::Yuv16 { planes: q, chroma: d, bits: m, .. }) => {
            n == m && c == d && p.iter().zip(q).all(|(x, y)| x == y)
        }
        _ => false,
    }
}

/// Where two frames of the same size and layout first differ: which plane and which sample, so a
/// wrong layout (stride, chroma order, sample shift) is recognisable from the report.
fn first_difference(a: &VideoFrame, b: &VideoFrame) -> String {
    match (&a.data, &b.data) {
        (PixelData::Yuv16 { planes: p, .. }, PixelData::Yuv16 { planes: q, .. }) => {
            for (i, (x, y)) in p.iter().zip(q.iter()).enumerate() {
                if let Some(at) = x.iter().zip(y.iter()).position(|(u, v)| u != v) {
                    return format!("{} 平面第 {} 个样本不同（硬解 {} / 软解 {}）", plane_name(i), at, x[at], y[at]);
                }
            }
            "平面长度不同（布局）".to_string()
        }
        (PixelData::Yuv8 { planes: p, .. }, PixelData::Yuv8 { planes: q, .. }) => {
            for (i, (x, y)) in p.iter().zip(q.iter()).enumerate() {
                if let Some(at) = x.iter().zip(y.iter()).position(|(u, v)| u != v) {
                    return format!("{} 平面第 {} 个样本不同（硬解 {} / 软解 {}）", plane_name(i), at, x[at], y[at]);
                }
            }
            "平面长度不同（布局）".to_string()
        }
        _ => "像素布局不同（色度格式或位深）".to_string(),
    }
}

/// `Y` / `U` / `V` for a plane index.
fn plane_name(i: usize) -> &'static str {
    match i {
        0 => "Y",
        1 => "U",
        _ => "V",
    }
}

/// Decode every embedded clip; called once at startup from `android_main`.
pub fn run() -> String {
    let mut out = run_one("H.264", CLIP_H264, false, 8);
    out.push_str(" ／ ");
    out.push_str(&run_one("HEVC", CLIP_HEVC, true, 8));
    out.push_str(" ／ ");
    out.push_str(&run_one("HEVC 10-bit", CLIP_HEVC10, true, 10));
    out
}
