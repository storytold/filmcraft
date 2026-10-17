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

// ---- VP9 (vpcC) ----

/// A VP9 fixture with hidden alt-ref frames (superframes), two GOPs: profile 0 8-bit or profile 2
/// 10-bit.
fn vp9_fixture(ff: &std::path::Path, profile: u8, size: &str) -> Option<std::path::PathBuf> {
    let (_, h) = size.split_once('x')?;
    let src = format!("testsrc2=s={size}:r=24:d=2,noise=alls=12:allf=t");
    let (name, pix) = if profile == 0 { (format!("vp9_p0_{h}.mp4"), "yuv420p") } else { (format!("vp9_p2_{h}.mp4"), "yuv420p10le") };
    let profile = profile.to_string();
    #[rustfmt::skip]
    let args = [
        "-f", "lavfi", "-i", &src, "-c:v", "libvpx-vp9", "-profile:v", &profile, "-b:v", "0", "-crf", "32",
        "-deadline", "good", "-cpu-used", "8", "-g", "24", "-auto-alt-ref", "1", "-lag-in-frames", "8",
        "-row-mt", "1", "-pix_fmt", pix,
    ];
    fixture(ff, &name, &args)
}

#[test]
fn vp9_is_bit_exact_with_the_software_decoder() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for profile in [0, 2] {
        for size in ["640x360", "1920x1080", "3840x2160"] {
            let name = format!("vp9 profile {profile} {size}");
            let Some(path) = vp9_fixture(&ff, profile, size) else { continue };
            let s = read_stream(&path);
            let Some(mut hw) = hardware(&s) else { continue };
            let mut sw = software(&s);
            let a = decode_all(hw.as_mut(), &s.samples);
            let b = decode_all(sw.as_mut(), &s.samples);
            assert!(a.len() >= 40, "{name}: {} pictures", a.len());
            assert_same(&name, &a, &b);
            assert_eq!(hw.name(), "VideoToolbox VP9", "{name}: stayed in hardware");
            // reset + reseek to the later key frame
            let k = (1..s.samples.len()).find(|&i| s.sync[i]).expect("two GOPs");
            hw.reset();
            sw.reset();
            assert_same(&format!("{name} from sample {k}"), &decode_all(hw.as_mut(), &s.samples[k..]), &decode_all(sw.as_mut(), &s.samples[k..]));
            for (smp, _) in &s.samples {
                assert_eq!(hw.is_random_access(smp), sw.is_random_access(smp), "{name}");
                assert_eq!(hw.is_disposable(smp), sw.is_disposable(smp), "{name}");
            }
        }
    }
}

/// PS5 recordings say limited range in `vpcC` and full range in the bitstream: the pictures are
/// still the software decoder's (no range conversion), with the bitstream's colour.
#[test]
fn vp9_container_range_differing_from_the_bitstream_is_bit_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = vp9_fixture(&ff, 0, "640x360") else { return };
    let mut s = read_stream(&path);
    let filmcraft_isobmff::CodecConfig::Vp9(c) = &mut s.entry.codec else { panic!("vpcC") };
    c.full_range = !c.full_range;
    let Some(mut hw) = hardware(&s) else { return };
    let a = decode_all(hw.as_mut(), &s.samples);
    assert_same("vp9 with a flipped vpcC range", &a, &decode_all(software(&s).as_mut(), &s.samples));
    assert_eq!(hw.name(), "VideoToolbox VP9");
}

#[test]
fn vp9_mid_stream_failure_and_damage_fall_back_to_software() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = vp9_fixture(&ff, 0, "640x360") else { return };
    let s = read_stream(&path);
    let filmcraft_isobmff::CodecConfig::Vp9(c) = &s.entry.codec else { panic!("vpcC") };
    let info = filmcraft_codecs::hw::FrameStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let Ok(_) = VtDecoder::new_vp9(info.clone(), c) else {
        eprintln!("SKIPPED: no VideoToolbox VP9 decoder");
        return;
    };
    let reference = decode_all(software(&s).as_mut(), &s.samples);
    for fail_at in [0u64, 5, 30] {
        let mut vt = VtDecoder::new_vp9(info.clone(), c).unwrap();
        vt.fail_after(fail_at);
        let mut d = HybridDecoder::new(Box::new(vt), s.entry.clone(), info.clone());
        let out = decode_all(&mut d, &s.samples);
        assert!(!d.is_hardware(), "switched to software");
        assert_same(&format!("vp9 failing at sample {fail_at}"), &out, &reference);
    }
    for seed in 1..=6u64 {
        let (samples, entry) = (s.samples.clone(), s.entry.clone());
        bounded(&format!("vp9 seed {seed}"), Duration::from_secs(60), move || {
            let mut seed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut d = filmcraft_platform::videotoolbox_factory(&entry).unwrap().unwrap();
            for (smp, pts) in samples.iter().take(40) {
                let mut m = smp.clone();
                let i = (xorshift(&mut seed) as usize) % m.len();
                m[i] ^= 1 << (xorshift(&mut seed) % 8);
                if xorshift(&mut seed).is_multiple_of(3) {
                    m.truncate(m.len() / 2 + 1);
                }
                let _ = d.decode(&m, *pts);
            }
            let _ = d.flush();
        });
    }
}
