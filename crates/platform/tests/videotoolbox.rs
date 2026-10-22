//! VideoToolbox decoding against our software decoders (macOS): every picture of H.264 High,
//! HEVC Main and HEVC Main 10 fixtures with B-frames must be identical (bit-exact planes, colour,
//! pixel aspect, pts and presentation order), also after `reset` + reseek and a mid-stream
//! `flush`; a forced mid-stream failure must continue in software with the software decoder's
//! exact output; damaged samples and parameter sets must give errors or fall back, never crash or
//! hang. Skips without ffmpeg (fixture generator) or without a hardware decoder.
#![cfg(target_os = "macos")]

mod common;

use std::time::Duration;

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_platform::HybridDecoder;
use filmcraft_platform::videotoolbox::VtDecoder;

/// The hardware decoder for a stream, or `None` (skip) when this machine has none for it.
fn hardware(s: &Stream) -> Option<Box<dyn VideoDecoder>> {
    match filmcraft_platform::videotoolbox_factory(&s.entry) {
        Some(Ok(d)) => {
            assert!(d.name().starts_with("VideoToolbox"), "{}", d.name());
            Some(d)
        }
        Some(Err(e)) => panic!("factory error: {e}"),
        None => {
            eprintln!("SKIPPED: no VideoToolbox hardware decoder for {}", s.entry.codec.name());
            None
        }
    }
}

fn software(s: &Stream) -> Box<dyn VideoDecoder> {
    filmcraft_codecs::software_video_decoder(&s.entry).unwrap()
}

#[test]
fn bit_exact_with_the_software_decoders() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let Some(mut hw) = hardware(&s) else { continue };
        let mut sw = software(&s);
        let a = decode_all(hw.as_mut(), &s.samples);
        let b = decode_all(sw.as_mut(), &s.samples);
        assert!(a.len() >= 60, "{name}: {} pictures", a.len());
        assert_same(name, &a, &b);
        assert!(hw.name().starts_with("VideoToolbox"), "{name}: stayed in hardware ({})", hw.name());

        // reset + reseek to each later sync sample, decode a stretch, flush mid-stream
        let syncs: Vec<usize> = (1..s.samples.len()).filter(|&i| s.sync[i]).collect();
        assert!(!syncs.is_empty(), "{name}: more than one GOP");
        for &k in syncs.iter().rev() {
            let end = (k + 17).min(s.samples.len());
            for d in [&mut hw, &mut sw] {
                d.reset();
            }
            let a = decode_all(hw.as_mut(), &s.samples[k..end]);
            let b = decode_all(sw.as_mut(), &s.samples[k..end]);
            assert!(!a.is_empty(), "{name}: pictures after seeking to {k}");
            assert_same(&format!("{name} from sample {k}"), &a, &b);
        }
        // back to the start after seeking around: the whole stream again
        hw.reset();
        sw.reset();
        assert_same(&format!("{name} after resets"), &decode_all(hw.as_mut(), &s.samples), &decode_all(sw.as_mut(), &s.samples));
        // random-access / disposable answers match the software decoder's
        for (smp, _) in &s.samples {
            assert_eq!(hw.is_random_access(smp), sw.is_random_access(smp), "{name}");
            assert_eq!(hw.is_disposable(smp), sw.is_disposable(smp), "{name}");
        }
    }
}

#[test]
fn mid_stream_failure_continues_in_software() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        let Ok(_) = VtDecoder::new(info.clone()) else {
            eprintln!("SKIPPED: no VideoToolbox hardware decoder for {name}");
            continue;
        };
        let reference = decode_all(software(&s).as_mut(), &s.samples);
        // fail at the first sample, inside the first GOP, at a sync sample and just after one
        let k = (1..s.samples.len()).find(|&i| s.sync[i]).unwrap();
        for fail_at in [0, 5, k, k + 1, k + 9] {
            let mut vt = VtDecoder::new(info.clone()).unwrap();
            vt.fail_after(fail_at as u64);
            let mut d = HybridDecoder::new(Box::new(vt), s.entry.clone(), info.clone());
            let before = filmcraft_codecs::hw::hw_stats().fallbacks;
            let out = decode_all(&mut d, &s.samples);
            assert!(!d.is_hardware(), "{name}: switched to software");
            assert!(filmcraft_codecs::hw::hw_stats().fallbacks > before, "{name}: fallback counted");
            assert_same(&format!("{name} failing at sample {fail_at}"), &out, &reference);
            // the instance stays in software across seeks
            d.reset();
            assert_same(&format!("{name} after the fallback and a reset"), &decode_all(&mut d, &s.samples), &reference);
        }
    }
}

/// Run `f` on a thread; fail if it panics or takes longer than `limit`.
fn bounded(what: &str, limit: Duration, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(r.is_ok());
    });
    match rx.recv_timeout(limit) {
        Ok(true) => {}
        Ok(false) => panic!("{what}: panicked"),
        Err(_) => panic!("{what}: hung"),
    }
}

#[test]
fn damaged_samples_and_parameter_sets_never_crash_or_hang() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        if hardware(&s).is_none() {
            continue;
        }
        // damaged samples (bit flips, truncation, corrupt length prefixes) through the hybrid
        for seed in 1..=6u64 {
            let samples = s.samples.clone();
            let entry = s.entry.clone();
            bounded(&format!("{name} seed {seed}"), Duration::from_secs(60), move || {
                let mut seed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                let mut d = filmcraft_platform::videotoolbox_factory(&entry).unwrap().unwrap();
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
                    if i % 13 == 12 {
                        d.reset();
                    }
                }
                let _ = d.flush();
            });
        }
        // damaged parameter sets: the factory declines or the decoder errors / falls back
        let rec = match &s.entry.codec {
            filmcraft_isobmff::CodecConfig::Avc(a) => a.to_bytes(),
            filmcraft_isobmff::CodecConfig::Hevc(c) => c.to_bytes(),
            _ => continue,
        };
        let hevc = matches!(s.entry.codec, filmcraft_isobmff::CodecConfig::Hevc(_));
        let mut seed = 0x1234_5678u64;
        for round in 0..24 {
            let mut r = rec.clone();
            if round % 3 == 2 {
                r.truncate(r.len() * (round + 1) / 30);
            } else {
                for _ in 0..1 + round % 4 {
                    let at = 6 + (xorshift(&mut seed) as usize) % r.len().saturating_sub(6).max(1);
                    if let Some(b) = r.get_mut(at) {
                        *b ^= 1 << (xorshift(&mut seed) % 8);
                    }
                }
            }
            let samples: Vec<_> = s.samples.iter().take(12).cloned().collect();
            bounded(&format!("{name} record round {round}"), Duration::from_secs(60), move || {
                let Some(info) = (if hevc { NalStreamInfo::from_hvcc(&r) } else { NalStreamInfo::from_avcc(&r) }).ok() else { return };
                let Ok(mut vt) = VtDecoder::new(info) else { return };
                for (smp, pts) in &samples {
                    if vt.decode(smp, *pts).is_err() {
                        break;
                    }
                }
                let _ = vt.flush();
            });
        }
    }
}

/// 4:2:2 (which our HEVC decoder does not decode): when VideoToolbox takes it, its pictures must
/// match ffmpeg's decode exactly; when it does not, the factory declines.
#[test]
fn hevc_422_10bit_matches_ffmpeg_when_the_hardware_takes_it() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=640x360:r=24:d=1,noise=alls=12:allf=t",
        "-c:v",
        "libx265",
        "-preset",
        "fast",
        "-tag:v",
        "hvc1",
        "-x265-params",
        "log-level=error:bframes=3:keyint=12:min-keyint=12:scenecut=0",
        "-pix_fmt",
        "yuv422p10le",
    ];
    let Some(path) = fixture(&ff, "hevc_422_10.mp4", &args) else { return };
    let s = read_stream(&path);
    let Some(mut hw) = hardware(&s) else { return };
    let out = decode_all(hw.as_mut(), &s.samples);
    let o =
        std::process::Command::new(&ff).args(["-v", "error", "-i", path.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "yuv422p10le", "-"]).output().unwrap();
    let (w, h) = (640usize, 360usize);
    let fsize = (w * h + 2 * (w / 2) * h) * 2;
    assert_eq!(out.len(), o.stdout.len() / fsize, "frame count");
    for (i, f) in out.iter().enumerate() {
        let raw: Vec<u16> = o.stdout[i * fsize..(i + 1) * fsize].as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let filmcraft_frame::PixelData::Yuv16 { planes, chroma, bits, .. } = &f.frame.data else { panic!("16-bit planes") };
        assert_eq!((*chroma, *bits), (filmcraft_frame::Chroma::C422, 10));
        let (y, c) = (w * h, (w / 2) * h);
        assert!(planes[0][..] == raw[..y] && planes[1][..] == raw[y..y + c] && planes[2][..] == raw[y + c..], "frame {i} differs from ffmpeg");
    }
}

/// #1750: interlaced H.264, which neither the Apple-silicon media engine nor our software decoder
/// decodes, goes to VideoToolbox's software decoder. Its frames are checked against ffmpeg's: the
/// right count, order and full (woven) frame size, every frame within 35 dB PSNR, also after a
/// reset and a reseek to a later sync sample, and without B-frames a bit-exact luma plane.
///
/// Not bit-exact: in MBAFF streams Apple's decoder and ffmpeg's disagree on the chroma of some
/// inter-predicted frames, and with B-frames (implicit weighted bi-prediction, temporal direct) on
/// the luma of a few as well, by a few levels (worst about 36 dB PSNR on these fixtures).
#[test]
fn interlaced_h264_is_close_to_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let (w, h) = (640usize, 360usize);
    // (fixture, x264 options, luma must be bit-exact)
    let cases = [
        ("h264_mbaff_tff_p.mp4", "tff=1:bframes=0:keyint=12:scenecut=0", true),
        ("h264_mbaff_bff_p.mp4", "bff=1:bframes=0:keyint=12:scenecut=0", true),
        ("h264_mbaff_tff_b.mp4", "tff=1:bframes=3:keyint=12:min-keyint=12:scenecut=0", false),
    ];
    for (name, opts, luma_exact) in cases {
        let args = [
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=25:d=2,noise=alls=12:allf=t",
            "-c:v",
            "libx264",
            "-flags",
            "+ildct+ilme",
            "-x264opts",
            opts,
            "-pix_fmt",
            "yuv420p",
        ];
        let Some(path) = fixture(&ff, name, &args) else { continue };
        let s = read_stream(&path);
        assert!(NalStreamInfo::from_entry(&s.entry).unwrap().unwrap().interlaced, "{name}: the fixture is interlaced");
        let mut vt = match filmcraft_platform::videotoolbox_factory(&s.entry) {
            Some(Ok(d)) => d,
            Some(Err(e)) => panic!("{name}: factory error: {e}"),
            // declined: a failure where VideoToolbox decodes H.264 at all (the progressive fixture)
            None => {
                let progressive = named(&ff, "h264_high.mp4").map(|p| read_stream(&p));
                assert!(
                    progressive.is_none_or(|p| filmcraft_platform::videotoolbox_factory(&p.entry).is_none()),
                    "{name}: VideoToolbox declined interlaced H.264 but takes progressive H.264"
                );
                eprintln!("SKIPPED: no VideoToolbox H.264 decoder");
                continue;
            }
        };
        let o =
            std::process::Command::new(&ff).args(["-v", "error", "-i", path.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]).output().unwrap();
        let reference: Vec<&[u8]> = o.stdout.chunks_exact(w * h * 3 / 2).collect();
        let check = |what: &str, f: &filmcraft_codecs::DecodedFrame, r: &[u8]| {
            let filmcraft_frame::PixelData::Yuv8 { planes, chroma, .. } = &f.frame.data else { panic!("{what}: 8-bit planes") };
            assert_eq!(*chroma, filmcraft_frame::Chroma::C420, "{what}");
            assert_eq!((f.frame.width as usize, f.frame.height as usize), (w, h), "{what}: woven frames, not fields");
            if luma_exact {
                assert!(planes[0][..] == r[..w * h], "{what}: luma differs from ffmpeg");
            }
            let ours: Vec<u8> = planes.iter().flat_map(|p| p.iter().copied()).collect();
            let mse = ours.iter().zip(r).map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2)).sum::<f64>() / ours.len() as f64;
            let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
            assert!(psnr >= 35.0, "{what}: {psnr:.1} dB from ffmpeg");
        };
        let out = decode_all(vt.as_mut(), &s.samples);
        assert_eq!(out.len(), reference.len(), "{name}: frame count");
        assert!(out.len() >= 50, "{name}: {} frames", out.len());
        assert!(out.windows(2).all(|p| p[0].pts < p[1].pts), "{name}: presentation order");
        for (i, (f, r)) in out.iter().zip(&reference).enumerate() {
            check(&format!("{name} frame {i}"), f, r);
        }
        // reset, then reseek to the last sync sample: the frames from there on are ffmpeg's last ones
        let k = (1..s.samples.len()).rev().find(|&i| s.sync[i]).expect("more than one GOP");
        vt.reset();
        let tail = decode_all(vt.as_mut(), &s.samples[k..]);
        let skip = reference.len() - tail.len();
        assert!(!tail.is_empty() && skip > 0, "{name}: {} frames after the reseek", tail.len());
        for (i, f) in tail.iter().enumerate() {
            check(&format!("{name} frame {} after the reseek", skip + i), f, reference[skip + i]);
        }
    }
}
