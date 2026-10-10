//! NVDEC decoding against our software decoders (Linux, NVIDIA's driver): every picture of the
//! shared H.264 High, HEVC Main (open GOP) and Main 10 fixtures and of 1080p streams must be
//! identical (bit-exact planes, colour, pixel aspect, pts and presentation order), also after
//! `reset` + reseek; a forced mid-stream failure must continue in software with the software
//! decoder's exact output; what the hardware path does not take is declined; damaged samples give
//! errors or fall back, never crash or hang. Skips without ffmpeg (fixture generator) or without
//! an NVIDIA GPU.
#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

mod common;

use std::time::Duration;

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
use filmcraft_platform::HybridDecoder;
use filmcraft_platform::nvdec::NvDecoder;

fn hardware(s: &Stream) -> Option<Box<dyn VideoDecoder>> {
    match filmcraft_platform::nvdec_factory(&s.entry) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => panic!("factory error: {e}"),
        None => {
            eprintln!("SKIPPED: no NVDEC hardware decoder for {}", s.entry.codec.name());
            None
        }
    }
}

fn software(s: &Stream) -> Box<dyn VideoDecoder> {
    filmcraft_codecs::software_video_decoder(&s.entry).unwrap()
}

fn streams(ff: &std::path::Path) -> Vec<(String, Stream)> {
    let mut out: Vec<_> =
        ["h264_high.mp4", "hevc_main.mp4", "hevc_main10.mp4"].iter().filter_map(|n| Some((n.to_string(), read_stream(&named(ff, n)?)))).collect();
    let src = "testsrc2=s=1920x1080:r=24:d=2,noise=alls=12:allf=t";
    let gop = "bframes=3:b-pyramid=normal:keyint=24:min-keyint=24:scenecut=0:ref=4";
    let x265 = format!("log-level=error:{}", gop.replace("normal", "1"));
    let cases: [(&str, Vec<&str>); 2] = [
        ("nv_h264_1080.mp4", vec!["-c:v", "libx264", "-profile:v", "high", "-preset", "veryfast", "-x264-params", gop, "-pix_fmt", "yuv420p"]),
        ("nv_hevc_main10_1080.mp4", vec!["-c:v", "libx265", "-preset", "ultrafast", "-tag:v", "hvc1", "-x265-params", &x265, "-pix_fmt", "yuv420p10le"]),
    ];
    for (name, enc) in cases {
        let args: Vec<&str> = ["-f", "lavfi", "-i", src].into_iter().chain(enc).collect();
        if let Some(path) = fixture(ff, name, &args) {
            out.push((name.to_string(), read_stream(&path)));
        }
    }
    out
}

#[test]
fn bit_exact_with_the_software_decoder() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, s) in streams(&ff) {
        let Some(mut hw) = hardware(&s) else { continue };
        let mut sw = software(&s);
        let a = decode_all(hw.as_mut(), &s.samples);
        assert!(a.len() >= 40, "{name}: {} pictures", a.len());
        assert_same(&name, &a, &decode_all(sw.as_mut(), &s.samples));
        // reset + reseek to each later sync sample. After a seek to a non-IDR H.264 I picture
        // (open GOP) the pictures shown before it are concealed differently: only those from the
        // seek point on must match. (HEVC drops such pictures, RASL, in both decoders.)
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        for k in (1..s.samples.len()).filter(|&i| s.sync[i]).rev() {
            let end = (k + 17).min(s.samples.len());
            hw.reset();
            sw.reset();
            let mut a = decode_all(hw.as_mut(), &s.samples[k..end]);
            let mut b = decode_all(sw.as_mut(), &s.samples[k..end]);
            if info.codec == NalCodec::H264 && !info.nal_types(&s.samples[k].0).contains(&5) {
                let start = s.samples[k].1;
                a.retain(|f| f.pts >= start);
                b.retain(|f| f.pts >= start);
            }
            assert!(!a.is_empty(), "{name}: pictures after seeking to {k}");
            assert_same(&format!("{name} from sample {k}"), &a, &b);
        }
        hw.reset();
        sw.reset();
        assert_same(&format!("{name} after resets"), &decode_all(hw.as_mut(), &s.samples), &decode_all(sw.as_mut(), &s.samples));
        assert!(hw.name().starts_with("NVDEC"), "{name}: stayed in hardware ({})", hw.name());
    }
}

#[test]
fn mid_stream_failure_continues_in_software() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, s) in streams(&ff) {
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        if NvDecoder::new(info.clone()).is_err() {
            eprintln!("SKIPPED: no NVDEC hardware decoder for {name}");
            continue;
        }
        let reference = decode_all(software(&s).as_mut(), &s.samples);
        let k = (1..s.samples.len()).find(|&i| s.sync[i]).unwrap();
        for fail_at in [0, 5, k, k + 1] {
            let mut nv = NvDecoder::new(info.clone()).unwrap();
            nv.fail_after(fail_at as u64);
            let mut d = HybridDecoder::new(Box::new(nv), s.entry.clone(), info.clone());
            let out = decode_all(&mut d, &s.samples);
            assert!(!d.is_hardware(), "{name}: switched to software");
            assert_same(&format!("{name} failing at sample {fail_at}"), &out, &reference);
        }
    }
}

#[test]
fn declines_what_the_hardware_path_does_not_take() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let src = "testsrc2=s=640x360:r=24:d=1";
    let cases: [(&str, Vec<&str>); 3] = [
        ("va_h264_interlaced.mp4", vec!["-c:v", "libx264", "-x264-params", "interlaced=1:keyint=12", "-pix_fmt", "yuv420p"]),
        ("va_h264_10bit.mp4", vec!["-c:v", "libx264", "-profile:v", "high10", "-pix_fmt", "yuv420p10le"]),
        ("va_hevc_422.mp4", vec!["-c:v", "libx265", "-tag:v", "hvc1", "-x265-params", "log-level=error:keyint=12", "-pix_fmt", "yuv422p10le"]),
    ];
    for (name, enc) in cases {
        let args: Vec<&str> = ["-f", "lavfi", "-i", src].into_iter().chain(enc).collect();
        let Some(path) = fixture(&ff, name, &args) else { continue };
        assert!(filmcraft_platform::nvdec_factory(&read_stream(&path).entry).is_none(), "{name}: declined");
    }
    let Some(path) = named(&ff, "h264_high.mp4") else { return };
    let s = read_stream(&path);
    filmcraft_codecs::hw::set_hardware_decoding(false);
    let off = filmcraft_platform::nvdec_factory(&s.entry).is_none();
    filmcraft_codecs::hw::set_hardware_decoding(true);
    assert!(off, "declined while hardware decoding is Off");
}

#[test]
fn damaged_samples_never_crash_or_hang() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, s) in streams(&ff).into_iter().take(3) {
        if hardware(&s).is_none() {
            continue;
        }
        for seed in 1..=8u64 {
            let (samples, entry) = (s.samples.clone(), s.entry.clone());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let r = std::panic::catch_unwind(move || {
                    let mut seed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                    // through the hybrid, and straight into the hardware decoder (no fallback)
                    let mut d = filmcraft_platform::nvdec_factory(&entry).unwrap().unwrap();
                    let mut raw = NvDecoder::new(NalStreamInfo::from_entry(&entry).unwrap().unwrap()).unwrap();
                    for (i, (smp, pts)) in samples.iter().enumerate().take(40) {
                        let mut m = smp.clone();
                        match xorshift(&mut seed) % 4 {
                            0 => {
                                for _ in 0..1 + xorshift(&mut seed) % 8 {
                                    let at = (xorshift(&mut seed) as usize) % m.len().max(1);
                                    if let Some(b) = m.get_mut(at) {
                                        *b ^= 1 << (xorshift(&mut seed) % 8);
                                    }
                                }
                            }
                            1 => m.truncate((xorshift(&mut seed) as usize) % m.len().max(1)),
                            2 if m.len() > 4 => m[..4].copy_from_slice(&(xorshift(&mut seed) as u32).to_be_bytes()),
                            _ => {}
                        }
                        let _ = d.decode(&m, *pts);
                        let _ = raw.decode(&m, *pts);
                        if i % 13 == 12 {
                            d.reset();
                            raw.reset();
                        }
                    }
                    let _ = d.flush();
                    let _ = raw.flush();
                });
                let _ = tx.send(r.is_ok());
            });
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(true) => {}
                Ok(false) => panic!("{name} seed {seed}: panicked"),
                Err(_) => panic!("{name} seed {seed}: hung"),
            }
        }
    }
}
