//! Media Foundation / Direct3D 11 decoding against our software decoders (Windows): every picture of H.264 High,
//! HEVC Main and HEVC Main 10 fixtures with B-frames must be identical (bit-exact planes, colour,
//! pixel aspect, pts and presentation order), also after `reset` + reseek and a mid-stream
//! `flush`; a forced mid-stream failure must continue in software with the software decoder's
//! exact output; damaged samples and parameter sets must give errors or fall back, never crash or
//! hang. Skips without ffmpeg (fixture generator) or without a hardware decoder.
#![cfg(target_os = "windows")]

mod common;

use std::time::Duration;

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_platform::HybridDecoder;
use filmcraft_platform::media_foundation::MfDecoder;

/// The hardware decoder for a stream, or `None` (skip) when this machine has none for it.
fn hardware(s: &Stream) -> Option<Box<dyn VideoDecoder>> {
    match filmcraft_platform::media_foundation_factory(&s.entry) {
        Some(Ok(d)) => {
            assert!(d.name().starts_with("Media Foundation"), "{}", d.name());
            Some(d)
        }
        Some(Err(e)) => panic!("factory error: {e}"),
        None => {
            eprintln!("SKIPPED: no Media Foundation hardware decoder for {}", s.entry.codec.name());
            None
        }
    }
}

fn software(s: &Stream) -> Box<dyn VideoDecoder> {
    filmcraft_codecs::software_video_decoder(&s.entry).unwrap()
}

/// Every picture of `s` from the hardware decoder equals the software decoder's: whole stream,
/// after `reset` + reseek to each later sync sample, after a mid-stream `flush`, and a full pass
/// after all the resets.
fn parity(name: &str, s: &Stream) {
    let Some(mut hw) = hardware(s) else { return };
    let mut sw = software(s);
    let a = decode_all(hw.as_mut(), &s.samples);
    let b = decode_all(sw.as_mut(), &s.samples);
    assert!(a.len() >= 40, "{name}: {} pictures", a.len());
    assert_same(name, &a, &b);
    assert!(hw.name().starts_with("Media Foundation"), "{name}: stayed in hardware ({})", hw.name());

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
    drained_decoder_restarts_only_at_idr(name, s);
    // random-access / disposable answers match the software decoder's
    for (smp, _) in &s.samples {
        assert_eq!(hw.is_random_access(smp), sw.is_random_access(smp), "{name}");
        assert_eq!(hw.is_disposable(smp), sw.is_disposable(smp), "{name}");
    }
}

/// After a `flush` (which drains the MFT) the run can only go on at an IDR picture; from the middle
/// of a GOP the decoder reports an error (the GOP cache seeks after every flush, and the hybrid
/// decoder would replay the run in software). After an IDR the pictures are the software decoder's.
fn drained_decoder_restarts_only_at_idr(name: &str, s: &Stream) {
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let is_idr = |i: usize| {
        info.nal_types(&s.samples[i].0).iter().any(|&t| if info.codec == filmcraft_codecs::hw::NalCodec::H264 { t == 5 } else { matches!(t, 19 | 20) })
    };
    let Ok(mut mf) = MfDecoder::new(info.clone()) else { return };
    let mid = s.samples.len() / 2;
    let Some(idr) = (mid..s.samples.len()).find(|&i| s.sync[i] && is_idr(i)) else { return };
    let mut sw = software(s);
    let before: Vec<_> = s.samples[..mid].iter().flat_map(|(d, p)| mf.decode(d, *p).unwrap()).collect();
    let drained = mf.flush();
    assert!(before.len() + drained.len() >= mid.saturating_sub(1), "{name}: flush returns the held pictures");
    if mid > 0 && !s.sync[mid] {
        assert!(mf.decode(&s.samples[mid].0, s.samples[mid].1).is_err(), "{name}: continuing mid-GOP after a flush is an error");
    }
    // flush, then an IDR: same pictures as software for the run that follows
    mf.reset();
    let _ = decode_all(&mut mf, &s.samples[..idr.min(mid + 2)]);
    let a = decode_all(&mut mf, &s.samples[idr..]);
    let b = decode_all(sw.as_mut(), &s.samples[idr..]);
    assert_same(&format!("{name} flush then IDR {idr}"), &a, &b);
}

#[test]
fn bit_exact_with_the_software_decoders() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        parity(name, &read_stream(&path));
    }
}

/// 1920x1080 and 3840x2160 with B-frames and two GOPs: H.264 High, HEVC Main (open GOP) and
/// HEVC Main 10.
fn large_fixture(ff: &std::path::Path, codec: &str, size: &str) -> Option<std::path::PathBuf> {
    let (w, h) = size.split_once('x')?;
    let src = format!("testsrc2=s={size}:r=24:d=2,noise=alls=12:allf=t");
    let (name, args): (String, Vec<&str>) = match codec {
        "h264" => (
            format!("h264_{h}.mp4"),
            vec![
                "-f",
                "lavfi",
                "-i",
                &src,
                "-c:v",
                "libx264",
                "-profile:v",
                "high",
                "-preset",
                "veryfast",
                "-x264-params",
                "bframes=3:b-pyramid=normal:keyint=24:min-keyint=24:scenecut=0",
                "-pix_fmt",
                "yuv420p",
            ],
        ),
        "hevc" => (
            format!("hevc_main_{h}.mp4"),
            vec![
                "-f",
                "lavfi",
                "-i",
                &src,
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-tag:v",
                "hvc1",
                "-x265-params",
                "log-level=error:bframes=4:keyint=24:min-keyint=24:scenecut=0:open-gop=1",
                "-pix_fmt",
                "yuv420p",
            ],
        ),
        _ => (
            format!("hevc_main10_{h}.mp4"),
            vec![
                "-f",
                "lavfi",
                "-i",
                &src,
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-profile:v",
                "main10",
                "-tag:v",
                "hvc1",
                "-x265-params",
                "log-level=error:bframes=4:keyint=24:min-keyint=24:scenecut=0",
                "-pix_fmt",
                "yuv420p10le",
            ],
        ),
    };
    let _ = w;
    fixture(ff, &name, &args)
}

fn large(codec: &str, size: &str) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = large_fixture(&ff, codec, size) else { return };
    parity(&format!("{codec} {size}"), &read_stream(&path));
}

#[test]
fn h264_1080p_and_2160p_are_bit_exact() {
    large("h264", "1920x1080");
    large("h264", "3840x2160");
}

#[test]
fn hevc_main_1080p_and_2160p_are_bit_exact() {
    large("hevc", "1920x1080");
    large("hevc", "3840x2160");
}

#[test]
fn hevc_main10_1080p_and_2160p_are_bit_exact() {
    large("hevc10", "1920x1080");
    large("hevc10", "3840x2160");
}

#[test]
fn mid_stream_failure_continues_in_software() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
        let Ok(_) = MfDecoder::new(info.clone()) else {
            eprintln!("SKIPPED: no Media Foundation hardware decoder for {name}");
            continue;
        };
        let reference = decode_all(software(&s).as_mut(), &s.samples);
        // fail at the first sample, inside the first GOP, at a sync sample and just after one
        let k = (1..s.samples.len()).find(|&i| s.sync[i]).unwrap();
        for fail_at in [0, 5, k, k + 1, k + 9] {
            let mut vt = MfDecoder::new(info.clone()).unwrap();
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
                let mut d = filmcraft_platform::media_foundation_factory(&entry).unwrap().unwrap();
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
                let Ok(mut vt) = MfDecoder::new(info) else { return };
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

/// 4:2:2 (which our HEVC decoder does not decode): when Media Foundation takes it, its pictures must
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

/// Creating and dropping decoders again and again (every clip on a timeline makes one) neither
/// fails nor leaks Direct3D / Media Foundation objects into the next one.
#[test]
fn repeated_initialisation() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = named(&ff, "h264_high.mp4") else { return };
    let s = read_stream(&path);
    let reference = decode_all(software(&s).as_mut(), &s.samples[..13]);
    for round in 0..40 {
        let Some(mut hw) = hardware(&s) else { return };
        if round % 8 == 0 {
            assert_same(&format!("round {round}"), &decode_all(hw.as_mut(), &s.samples[..13]), &reference);
        }
    }
}

/// Several decoders at once on different threads (the frame workers) share the device and give the
/// software decoder's pictures.
#[test]
fn concurrent_decoders_share_the_device() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let jobs: Vec<_> = ["h264_high.mp4", "hevc_main.mp4", "hevc_main10.mp4", "h264_high.mp4", "hevc_main.mp4"]
        .into_iter()
        .filter_map(|n| named(&ff, n))
        .map(|path| {
            std::thread::spawn(move || {
                let s = read_stream(&path);
                let Some(mut hw) = hardware(&s) else { return };
                let n = s.samples.len().min(40);
                let a = decode_all(hw.as_mut(), &s.samples[..n]);
                hw.reset();
                let b = decode_all(hw.as_mut(), &s.samples[..n]);
                let c = decode_all(software(&s).as_mut(), &s.samples[..n]);
                assert_same("concurrent", &a, &c);
                assert_same("concurrent, after a reset", &b, &c);
            })
        })
        .collect();
    for j in jobs {
        j.join().unwrap();
    }
}

/// A decoder dropped, used and moved between threads (decoders migrate between frame workers).
#[test]
fn decoder_moves_between_threads() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = named(&ff, "hevc_main.mp4") else { return };
    let s = read_stream(&path);
    let Some(mut hw) = hardware(&s) else { return };
    let reference = decode_all(software(&s).as_mut(), &s.samples[..30]);
    let samples = s.samples.clone();
    let (hw, out) = std::thread::spawn(move || {
        let out = decode_all(hw.as_mut(), &samples[..30]);
        (hw, out)
    })
    .join()
    .unwrap();
    assert_same("first thread", &out, &reference);
    let mut hw = hw;
    hw.reset();
    let samples = s.samples.clone();
    let out = std::thread::spawn(move || decode_all(hw.as_mut(), &samples[..30])).join().unwrap();
    assert_same("second thread", &out, &reference);
}
