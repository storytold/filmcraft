//! NVDEC decoding (Windows, 64-bit Linux) of 4:2:2 and 10-bit H.264 (camera footage: Sony XAVC S-I
//! and S, XAVC HS, Canon, Panasonic) against our software decoders, on GPUs whose NVDEC takes those
//! formats (4:2:2 needs Blackwell, RTX 50 series): every picture must be identical. HEVC 4:2:2
//! (XAVC HS), which has no software decoder yet, and the H.264 formats are also compared with
//! ffmpeg's software decode (PSNR). Streams the GPU declines are skipped. Needs ffmpeg for the
//! fixtures and an NVIDIA GPU.
#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

mod common;

use std::process::{Command, Stdio};

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_frame::{Chroma, PixelData};
use filmcraft_platform::nvdec::NvDecoder;

fn software(s: &Stream) -> Option<Box<dyn VideoDecoder>> {
    filmcraft_codecs::software_video_decoder(&s.entry).ok()
}

fn check(name: &str, enc: &[&str]) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let src = "testsrc2=s=640x360:r=24:d=2,noise=alls=12:allf=t";
    let args: Vec<&str> = ["-f", "lavfi", "-i", src].into_iter().chain(enc.iter().copied()).collect();
    let Some(path) = fixture(&ff, name, &args) else { panic!("could not write {name}") };
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut hw = match NvDecoder::new(info) {
        Ok(d) => d,
        Err(why) => {
            eprintln!("SKIPPED {name}: NVDEC declines it ({why})");
            return;
        }
    };
    let Some(mut sw) = software(&s) else {
        eprintln!("SKIPPED {name}: no software decoder to compare with");
        return;
    };
    let a = decode_all(&mut hw, &s.samples);
    assert!(a.len() >= 40, "{name}: {} pictures", a.len());
    assert_same(name, &a, &decode_all(sw.as_mut(), &s.samples));
    hw.reset();
    sw.reset();
    assert_same(&format!("{name} after reset"), &decode_all(&mut hw, &s.samples), &decode_all(sw.as_mut(), &s.samples));
}

#[test]
fn h264_422_10_bit_intra_equals_software() {
    check("nv_h264_422_10_intra.mp4", &["-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le", "-x264-params", "keyint=1:cabac=0"]);
    check("nv_h264_422_10_intra_cabac.mp4", &["-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le", "-x264-params", "keyint=1:cabac=1"]);
}

#[test]
fn h264_422_10_bit_long_gop_equals_software() {
    check(
        "nv_h264_422_10_gop.mp4",
        &["-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le", "-x264-params", "bframes=3:keyint=24:min-keyint=24:scenecut=0"],
    );
}

#[test]
fn h264_422_8_bit_equals_software() {
    check(
        "nv_h264_422_8_gop.mp4",
        &["-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p", "-x264-params", "bframes=2:keyint=24:min-keyint=24:scenecut=0"],
    );
}

#[test]
fn h264_420_10_bit_equals_software() {
    check(
        "nv_h264_420_10_gop.mp4",
        &["-c:v", "libx264", "-profile:v", "high10", "-pix_fmt", "yuv420p10le", "-x264-params", "bframes=2:keyint=24:min-keyint=24:scenecut=0"],
    );
}

/// Timing of a real camera clip (`FILMCRAFT_NVDEC_CLIP=cut.mp4`): session creation, first picture and
/// steady state, one decoder.
#[test]
#[ignore = "needs FILMCRAFT_NVDEC_CLIP"]
fn camera_clip_timing() {
    let Some(path) = std::env::var_os("FILMCRAFT_NVDEC_CLIP").map(std::path::PathBuf::from) else { return };
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let t = std::time::Instant::now();
    let mut hw = NvDecoder::new(info).unwrap();
    eprintln!("create: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let first = hw.decode(&s.samples[0].0, s.samples[0].1).unwrap().len() + hw.flush().len();
    eprintln!("first picture ({first}): {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let mut n = 0;
    for (data, pts) in &s.samples[1..] {
        n += hw.decode(data, *pts).unwrap().len();
    }
    n += hw.flush().len();
    eprintln!("{n} pictures in {:?} ({:.1} ms each)", t.elapsed(), t.elapsed().as_secs_f64() * 1000.0 / n.max(1) as f64);
}

/// Several decoders at once (what the frame workers do): throughput and, run under a CPU-time
/// meter, the CPU they burn (`FILMCRAFT_NVDEC_CLIP=cut.mp4`, `FILMCRAFT_NVDEC_THREADS=6`).
#[test]
#[ignore = "needs FILMCRAFT_NVDEC_CLIP"]
fn camera_clip_concurrent_sessions() {
    let Some(path) = std::env::var_os("FILMCRAFT_NVDEC_CLIP").map(std::path::PathBuf::from) else { return };
    let threads: usize = std::env::var("FILMCRAFT_NVDEC_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let t = std::time::Instant::now();
    let total: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|k| {
                let (s, info) = (&s, info.clone());
                scope.spawn(move || {
                    let mut hw = NvDecoder::new(info).unwrap();
                    let mut n = 0;
                    for (data, pts) in s.samples.iter().skip(k).step_by(threads) {
                        n += hw.decode(data, *pts).unwrap().len();
                    }
                    n
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });
    eprintln!("{threads} sessions: {total} pictures in {:?} ({:.0} fps)", t.elapsed(), total as f64 / t.elapsed().as_secs_f64());
}

/// An all-intra stream (Intra profile) is reported as such by the hybrid decoder, and each sample
/// returns its own picture from the call that fed it: separate decoders can then decode separate
/// frames in parallel (`GopCache`), in hardware and, after a failure, in software.
#[test]
fn all_intra_streams_return_one_picture_per_call() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let src = "testsrc2=s=640x360:r=24:d=1,noise=alls=12:allf=t";
    let args = ["-f", "lavfi", "-i", src, "-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le", "-x264-params", "keyint=1:cabac=0"];
    let path = fixture(&ff, "nv_h264_422_10_intra_hybrid.mp4", &args).expect("fixture");
    let s = read_stream(&path);
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    assert!(info.is_all_intra());
    for fail_after in [None, Some(3u64)] {
        let Ok(mut hw) = NvDecoder::new(info.clone()) else {
            eprintln!("SKIPPED: NVDEC declines the stream");
            return;
        };
        if let Some(n) = fail_after {
            hw.fail_after(n);
        }
        let mut d = filmcraft_platform::HybridDecoder::new(Box::new(hw), s.entry.clone(), info.clone());
        assert!(d.intra_only());
        for (k, (data, pts)) in s.samples.iter().enumerate() {
            let out = d.decode(data, *pts).unwrap();
            assert_eq!(out.len(), 1, "sample {k} (hardware failing after {fail_after:?}): pictures");
            assert_eq!(out[0].pts, *pts);
        }
        assert_eq!(d.is_hardware(), fail_after.is_none());
    }
}

/// ffmpeg's software decode of `path` as planar `pix_fmt` frames of `frame_bytes` each, in
/// presentation order.
fn ffmpeg_frames(ff: &std::path::Path, path: &std::path::Path, pix_fmt: &str, frame_bytes: usize) -> Vec<Vec<u8>> {
    let out = Command::new(ff)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "ffmpeg decode of {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len() % frame_bytes, 0, "{}: whole frames", path.display());
    out.stdout.chunks(frame_bytes).map(<[u8]>::to_vec).collect()
}

/// PSNR in dB of `a` against `b` for samples of `bits` bits (infinite when identical).
fn psnr(a: &[u16], b: &[u16], bits: u32) -> f64 {
    assert_eq!(a.len(), b.len());
    let se: f64 = a.iter().zip(b).map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2)).sum();
    let peak = f64::from((1u32 << bits) - 1);
    if se == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak * a.len() as f64 / se).log10() }
}

/// Sony XAVC S-I (all-intra H.264 High 4:2:2 10-bit), XAVC S 4:2:2 (long GOP), H.264 4:2:2 8-bit
/// and High 10, and XAVC HS 4:2:2 (HEVC Main 4:2:2 10): every picture NVDEC decodes must match
/// ffmpeg's software decode, each plane at 60 dB or more (hardware decoders are bit-exact in
/// practice), also after a reset, with the stream's chroma format and depth.
#[test]
fn camera_formats_match_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let src = "testsrc2=s=640x360:r=24:d=2,noise=alls=12:allf=t";
    let gop = "bframes=3:b-pyramid=normal:keyint=24:min-keyint=24:scenecut=0";
    let x265 = "log-level=error:bframes=3:b-pyramid=1:keyint=24:min-keyint=24:scenecut=0";
    let cases: [(&str, Vec<&str>, &str, Chroma, u32); 5] = [
        ("nv_xavc_s_i_h264_422_10.mp4", vec!["-c:v", "libx264", "-profile:v", "high422", "-x264-params", "keyint=1"], "yuv422p10le", Chroma::C422, 10),
        ("nv_xavc_s_h264_422_10.mp4", vec!["-c:v", "libx264", "-profile:v", "high422", "-x264-params", gop], "yuv422p10le", Chroma::C422, 10),
        ("nv_h264_422_8_pyramid.mp4", vec!["-c:v", "libx264", "-profile:v", "high422", "-x264-params", gop], "yuv422p", Chroma::C422, 8),
        ("nv_h264_high10_pyramid.mp4", vec!["-c:v", "libx264", "-profile:v", "high10", "-x264-params", gop], "yuv420p10le", Chroma::C420, 10),
        (
            "nv_xavc_hs_hevc_422_10.mp4",
            vec!["-c:v", "libx265", "-profile:v", "main422-10", "-tag:v", "hvc1", "-x265-params", x265],
            "yuv422p10le",
            Chroma::C422,
            10,
        ),
    ];
    for (name, enc, pix_fmt, chroma, bits) in cases {
        let args: Vec<&str> = ["-f", "lavfi", "-i", src].into_iter().chain(enc).chain(["-pix_fmt", pix_fmt]).collect();
        let Some(path) = fixture(&ff, name, &args) else { panic!("could not write {name}") };
        let s = read_stream(&path);
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        assert_eq!((info.bit_depth_luma, info.chroma_format_idc), (bits, if chroma == Chroma::C422 { 2 } else { 1 }), "{name}: stream info");
        let mut hw = match NvDecoder::new(info) {
            Ok(d) => d,
            Err(why) => {
                eprintln!("SKIPPED {name}: NVDEC declines it ({why})");
                continue;
            }
        };
        let (w, h) = (640usize, 360usize);
        let bps = if bits > 8 { 2 } else { 1 };
        let (luma, plane) = (w * h, w / 2 * if chroma == Chroma::C422 { h } else { h / 2 });
        let reference = ffmpeg_frames(&ff, &path, pix_fmt, (luma + 2 * plane) * bps);
        let widen = |b: &[u8]| -> Vec<u16> {
            if bps == 1 { b.iter().map(|&v| u16::from(v)).collect() } else { b.as_chunks::<2>().0.iter().map(|&p| u16::from_le_bytes(p)).collect() }
        };
        let first = decode_all(&mut hw, &s.samples);
        hw.reset();
        let again = decode_all(&mut hw, &s.samples);
        let mut worst = f64::INFINITY;
        for (pass, frames) in [("", &first), (" after reset", &again)] {
            assert_eq!(frames.len(), reference.len(), "{name}{pass}: picture count");
            assert!(frames.windows(2).all(|p| p[0].pts < p[1].pts), "{name}{pass}: presentation order");
            for (i, (f, r)) in frames.iter().zip(&reference).enumerate() {
                assert_eq!((f.frame.width, f.frame.height), (w as u32, h as u32), "{name}{pass} #{i}: size");
                let planes: [Vec<u16>; 3] = match &f.frame.data {
                    PixelData::Yuv8 { planes, chroma: c, .. } if bits == 8 && *c == chroma => planes.clone().map(|p| p.iter().map(|&v| u16::from(v)).collect()),
                    PixelData::Yuv16 { planes, chroma: c, bits: b, .. } if *b == bits && *c == chroma => planes.clone().map(|p| p.to_vec()),
                    _ => panic!("{name}{pass} #{i}: unexpected pixel format {}", f.frame.format_label()),
                };
                let refs = [&r[..luma * bps], &r[luma * bps..(luma + plane) * bps], &r[(luma + plane) * bps..]].map(widen);
                for (p, (a, b)) in planes.iter().zip(&refs).enumerate() {
                    let db = psnr(a, b, bits);
                    assert!(db >= 60.0, "{name}{pass} #{i} plane {p}: {db:.1} dB");
                    worst = worst.min(db);
                }
            }
        }
        eprintln!("{name}: {} pictures through {}, worst plane PSNR against ffmpeg {worst} dB", first.len(), hw.name());
    }
}
