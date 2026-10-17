//! NVDEC decoding of 4:2:2 and 10-bit H.264 / HEVC (camera footage: Sony XAVC S-I and S, XAVC HS,
//! Canon, Panasonic) against our software decoders, on GPUs whose NVDEC takes those formats (4:2:2
//! needs Blackwell, RTX 50 series): every picture must be identical. Streams the GPU declines are
//! skipped. Needs ffmpeg for the fixtures and an NVIDIA GPU.
#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

mod common;

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::NalStreamInfo;
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
