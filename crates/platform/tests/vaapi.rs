//! VA-API decoding against our software decoders (Linux): every picture of the H.264 High, HEVC
//! Main, HEVC Main 10, VP9 profile 0 / 2 and AV1 8- / 10-bit fixtures must be identical (bit-exact planes, colour, pixel aspect, pts and presentation order), also
//! after `reset` + reseek; damaged samples must give errors or fall back, never crash or hang.
//! Skips without ffmpeg (fixture generator) or without a VA-API driver that decodes H.264.
#![cfg(target_os = "linux")]

mod common;

use common::*;
use filmcraft_codecs::VideoDecoder;

/// The hardware counters are process-wide: tests that read them must not overlap.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// VP9 fixtures (hidden alt-ref frames in superframes, two GOPs; libvpx is a generator only):
/// profile 0 (8-bit) and profile 2 (10-bit).
const VP9: &[(&str, &str, &str)] = &[("vp9_p0.mp4", "0", "yuv420p"), ("vp9_p2.mp4", "2", "yuv420p10le")];

fn vp9_fixture(ff: &std::path::Path, name: &str, profile: &str, pix: &str) -> Option<std::path::PathBuf> {
    vp9_fixture_sized(ff, name, profile, pix, "640x360")
}

fn vp9_fixture_sized(ff: &std::path::Path, name: &str, profile: &str, pix: &str, size: &str) -> Option<std::path::PathBuf> {
    let src = format!("testsrc2=s={size}:r=24:d=3,noise=alls=12:allf=t");
    let args = [
        "-f",
        "lavfi",
        "-i",
        &src,
        "-c:v",
        "libvpx-vp9",
        "-profile:v",
        profile,
        "-b:v",
        "0",
        "-crf",
        "32",
        "-deadline",
        "good",
        "-cpu-used",
        "8",
        "-g",
        "24",
        "-auto-alt-ref",
        "1",
        "-lag-in-frames",
        "8",
        "-pix_fmt",
        pix,
    ];
    fixture(ff, name, &args)
}

/// AV1 fixtures (libaom: hidden alt-ref frames and show_existing_frame, two GOPs): 8- and 10-bit.
const AV1: &[(&str, &str)] = &[("av1_8bit.mp4", "yuv420p"), ("av1_10bit.mp4", "yuv420p10le")];

fn av1_fixture(ff: &std::path::Path, name: &str, pix: &str) -> Option<std::path::PathBuf> {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=640x360:r=24:d=3,noise=alls=12:allf=t",
        "-c:v",
        "libaom-av1",
        "-crf",
        "34",
        "-b:v",
        "0",
        "-cpu-used",
        "8",
        "-row-mt",
        "1",
        "-g",
        "24",
        "-lag-in-frames",
        "8",
        "-pix_fmt",
        pix,
    ];
    fixture(ff, name, &args)
}

/// Every fixture this test decodes: (name, path).
fn fixtures(ff: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let nal = FIXTURES.iter().filter_map(|(name, _)| Some((name.to_string(), named(ff, name)?)));
    let vp9 = VP9.iter().filter_map(|(name, p, pix)| Some((name.to_string(), vp9_fixture(ff, name, p, pix)?)));
    let av1 = AV1.iter().filter_map(|(name, pix)| Some((name.to_string(), av1_fixture(ff, name, pix)?)));
    // SVT-AV1 (most AV1 in the wild): other tools than libaom's (CDEF / restoration choices,
    // 64x64 superblocks, its own GOP structure); skipped when this ffmpeg lacks it
    let svt_args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1280x720:r=24:d=3,noise=alls=12:allf=t",
        "-c:v",
        "libsvtav1",
        "-preset",
        "8",
        "-crf",
        "35",
        "-g",
        "24",
        "-pix_fmt",
        "yuv420p10le",
    ];
    let svt = fixture(ff, "av1_svt_10bit.mp4", &svt_args).map(|p| ("av1_svt_10bit.mp4".to_string(), p));
    nal.chain(vp9).chain(av1).chain(svt).collect()
}

/// The hardware decoder for a stream, or `None` (skip) when this machine has none for it.
fn hardware(s: &Stream) -> Option<Box<dyn VideoDecoder>> {
    filmcraft_codecs::hw::set_hardware_decoding(true);
    match filmcraft_platform::vaapi_factory(&s.entry) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => panic!("factory error: {e}"),
        None => {
            eprintln!("SKIPPED: no VA-API hardware decoder for {}", s.entry.codec.name());
            None
        }
    }
}

fn software(s: &Stream) -> Box<dyn VideoDecoder> {
    filmcraft_codecs::software_video_decoder(&s.entry).unwrap()
}

#[test]
fn bit_exact_with_the_software_decoders() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, path) in fixtures(&ff) {
        let name = name.as_str();
        let s = read_stream(&path);
        let Some(mut hw) = hardware(&s) else { continue };
        let mut sw = software(&s);
        let before = filmcraft_codecs::hw::hw_stats();
        let a = decode_all(hw.as_mut(), &s.samples);
        let b = decode_all(sw.as_mut(), &s.samples);
        assert!(a.len() >= 60, "{name}: {} pictures", a.len());
        assert_same(name, &a, &b);
        let after = filmcraft_codecs::hw::hw_stats();
        // the pictures really came from the GPU (stats are global: compare this run only)
        assert_eq!(after.fallbacks - before.fallbacks, 0, "{name} stayed in hardware: {before:?} -> {after:?}");
        assert_eq!(after.frames - before.frames, a.len() as u64, "{name}: every picture decoded in hardware");
        // reset + reseek to each later sync sample, decode a stretch
        let syncs: Vec<usize> = (1..s.samples.len()).filter(|&i| s.sync[i]).collect();
        assert!(!syncs.is_empty(), "{name}: more than one GOP");
        for &k in syncs.iter().rev() {
            let end = (k + 17).min(s.samples.len());
            hw.reset();
            sw.reset();
            let a = decode_all(hw.as_mut(), &s.samples[k..end]);
            let b = decode_all(sw.as_mut(), &s.samples[k..end]);
            assert!(!a.is_empty(), "{name}: pictures after seeking to {k}");
            assert_same(&format!("{name} from sample {k}"), &a, &b);
        }
        hw.reset();
        sw.reset();
        assert_same(&format!("{name} after resets"), &decode_all(hw.as_mut(), &s.samples), &decode_all(sw.as_mut(), &s.samples));
        eprintln!("{name}: {} pictures bit-exact in hardware", a.len());
    }
}

#[test]
fn damaged_samples_never_crash() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let ff = filmcraft_testkit::require_ffmpeg!();
    let mut seed = 0x5eed_u64;
    for (_, path) in fixtures(&ff) {
        let s = read_stream(&path);
        for round in 0..4 {
            let Some(mut hw) = hardware(&s) else { continue };
            for (i, (smp, pts)) in s.samples.iter().enumerate() {
                let mut d = smp.clone();
                if i > 0 && !d.is_empty() && round > 0 {
                    for _ in 0..round * 3 {
                        let at = (xorshift(&mut seed) as usize) % d.len();
                        d[at] ^= (xorshift(&mut seed) & 0xff) as u8;
                    }
                    if round == 3 {
                        d.truncate(d.len() / 2);
                    }
                }
                let _ = hw.decode(&d, *pts);
            }
            let _ = hw.flush();
        }
    }
}

/// A key frame of another size (VP9) / a sequence header of another picture format (AV1) in the
/// middle of a stream: the hybrid decoder continues with the software decoder, whose output is the
/// reference.
#[test]
fn in_band_format_changes_fall_back_to_software() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let ff = filmcraft_testkit::require_ffmpeg!();
    let all = fixtures(&ff);
    let path = |n: &str| all.iter().find(|(name, _)| name == n).map(|(_, p)| p.clone());
    let vp9_720 = vp9_fixture_sized(&ff, "vp9_p0_720.mp4", "0", "yuv420p", "1280x720");
    let pairs = [("vp9", vp9_720, path("vp9_p0.mp4")), ("av1", path("av1_svt_10bit.mp4"), path("av1_10bit.mp4"))];
    for (codec, big, small) in pairs {
        let (Some(big), Some(small)) = (big, small) else { continue };
        let (a, b) = (read_stream(&big), read_stream(&small));
        let Some(mut hw) = hardware(&a) else { continue };
        let mut sw = software(&a);
        // (the second stream's pictures come later in time, as in a real stream)
        let samples: Vec<_> = a.samples[..8].iter().cloned().chain(b.samples[..8].iter().map(|(d, p)| (d.clone(), p + 100_000))).collect();
        let before = filmcraft_codecs::hw::hw_stats().fallbacks;
        let got = decode_all(hw.as_mut(), &samples);
        let want = decode_all(sw.as_mut(), &samples);
        assert!(filmcraft_codecs::hw::hw_stats().fallbacks > before, "{codec}: the change was counted as a fallback");
        assert_same(&format!("{codec} 720p then 360p"), &got, &want);
        assert!(got.iter().any(|f| f.frame.width == 640) && got.iter().any(|f| f.frame.width == 1280), "{codec}: both sizes came out");
    }
}
