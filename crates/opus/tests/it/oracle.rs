//! ffmpeg/libopus oracle tests (skipped when ffmpeg with libopus is unavailable).
//!
//! Synthetic signals are encoded with `ffmpeg -c:a libopus` across bitrates, frame sizes,
//! applications, channel layouts and VBR/CBR, decoded by libopus (through ffmpeg, as an external
//! oracle only) to f32, and compared with our decoder: sample-accurate length after pre-skip and
//! end trimming, plus SNR. CELT-only streams are expected to match to floating-point precision;
//! streams containing SILK differ by the (non-normative) SILK resampler, so they are held to a
//! lower SNR bound and the spectral quality metric.

use crate::common;

use std::process::Command;

use filmcraft_opus::{Decoder, Mode, OpusHead, Packet};

fn has_libopus(ff: &std::path::Path) -> bool {
    Command::new(ff).args(["-hide_banner", "-encoders"]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains("libopus")).unwrap_or(false)
}

struct Case {
    name: &'static str,
    channels: usize,
    signal: fn(usize, u64) -> Vec<f32>,
    args: &'static [&'static str],
    min_snr_celt: f64,
}

fn sweep_sig(n: usize, _: u64) -> Vec<f32> {
    common::sweep(n, 0.5)
}

fn run(case: &Case, rate: u32) -> (f64, f64, Vec<Mode>) {
    let ff = common::ffmpeg().unwrap();
    let dir = common::fixtures();
    let secs = 3;
    let n = 48000 * secs;
    let mut pcm = vec![0f32; n * case.channels];
    for c in 0..case.channels {
        let s = (case.signal)(n, 7 + c as u64);
        for i in 0..n {
            // Decorrelate channels a little (delay + gain) so stereo coding is exercised.
            let j = i.saturating_sub(c * 37);
            pcm[i * case.channels + c] = s[j] * (1.0 - 0.1 * c as f32);
        }
    }
    let raw = dir.join(format!("{}.f32", case.name));
    let opus = dir.join(format!("{}.opus", case.name));
    let dec = dir.join(format!("{}.dec.f32", case.name));
    std::fs::write(&raw, pcm.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    let layout = match case.channels {
        1 => "mono",
        2 => "stereo",
        6 => "5.1",
        _ => unreachable!(),
    };
    let mut cmd = Command::new(&ff);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "f32le", "-ar", "48000", "-ch_layout", layout, "-i"]).arg(&raw);
    cmd.args(["-c:a", "libopus"]).args(case.args).arg(&opus);
    assert!(cmd.status().unwrap().success(), "ffmpeg encode failed for {}", case.name);
    let st = Command::new(&ff)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-c:a", "libopus", "-i"])
        .arg(&opus)
        .args(["-f", "f32le", "-ar", &rate.to_string()])
        .arg(&dec)
        .status()
        .unwrap();
    assert!(st.success());
    let reference: Vec<f32> = std::fs::read(&dec).unwrap().as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();

    let (packets, granule) = common::ogg_packets(&std::fs::read(&opus).unwrap());
    let mut d = Decoder::from_head(OpusHead::parse(&packets[0]).unwrap(), rate).unwrap();
    assert_eq!(d.channels(), case.channels);
    d.set_trim_pre_skip(true);
    let mut planar = vec![Vec::new(); case.channels];
    let mut modes = Vec::new();
    for p in &packets[2..] {
        if let Ok(pk) = Packet::parse(p)
            && !modes.contains(&pk.toc.mode)
        {
            modes.push(pk.toc.mode);
        }
        let out = d.decode(Some(p)).unwrap();
        for (c, o) in out.into_iter().enumerate() {
            planar[c].extend(o);
        }
    }
    // End trimming from the final granule position.
    let total = ((granule as usize).saturating_sub(d.head().pre_skip as usize)) * rate as usize / 48000;
    for p in &mut planar {
        assert!(p.len() >= total, "{}: decoded {} < granule length {}", case.name, p.len(), total);
        p.truncate(total);
    }
    // Our output is in Vorbis channel order; ffmpeg reorders 5.1 to FL FR FC LFE BL BR.
    let order: Vec<usize> = if case.channels == 6 { vec![0, 2, 1, 5, 3, 4] } else { (0..case.channels).collect() };
    let ours: Vec<f32> = (0..total).flat_map(|i| order.iter().map(|&c| planar[c][i]).collect::<Vec<_>>()).collect();
    if std::env::var("DIAG").is_ok_and(|v| v == case.name) {
        let blk = 12000 * case.channels;
        for (b, (r, o)) in reference.chunks(blk).zip(ours.chunks(blk)).enumerate() {
            let er: f32 = r.iter().map(|v| v * v).sum();
            let eo: f32 = o.iter().map(|v| v * v).sum();
            eprintln!("{:5.2}s ref {:9.3e} ours {:9.3e} snr {:6.1}", b as f32 * 0.25, er, eo, common::snr_db(r, o));
        }
    }
    assert_eq!(ours.len(), reference.len(), "{}: length mismatch vs libopus (samples/ch {} vs {})", case.name, total, reference.len() / case.channels);
    let snr = common::snr_db(&reference, &ours);
    let q = if case.channels <= 2 { common::opus_quality(&reference, &ours, case.channels, rate) } else { f64::NAN };
    let _ = std::fs::remove_file(&raw);
    let _ = std::fs::remove_file(&dec);
    let _ = std::fs::remove_file(&opus);
    (snr, q, modes)
}

#[test]
fn libopus_oracle() {
    let Some(ff) = common::ffmpeg() else {
        eprintln!("ffmpeg not found; skipping");
        return;
    };
    if !has_libopus(&ff) {
        eprintln!("ffmpeg without libopus; skipping");
        return;
    }
    let cases = [
        Case { name: "speech_mono_6k_voip", channels: 1, signal: common::speech, args: &["-b:a", "6k", "-application", "voip"], min_snr_celt: 0.0 },
        Case {
            name: "speech_mono_12k_voip_60ms",
            channels: 1,
            signal: common::speech,
            args: &["-b:a", "12k", "-application", "voip", "-frame_duration", "60"],
            min_snr_celt: 0.0,
        },
        Case {
            name: "speech_mono_16k_voip_40ms",
            channels: 1,
            signal: common::speech,
            args: &["-b:a", "16k", "-application", "voip", "-frame_duration", "40"],
            min_snr_celt: 0.0,
        },
        Case { name: "speech_stereo_24k_voip", channels: 2, signal: common::speech, args: &["-b:a", "24k", "-application", "voip"], min_snr_celt: 0.0 },
        Case { name: "music_stereo_32k_audio", channels: 2, signal: common::music, args: &["-b:a", "32k", "-application", "audio"], min_snr_celt: 0.0 },
        Case { name: "music_stereo_64k_audio_10ms", channels: 2, signal: common::music, args: &["-b:a", "64k", "-frame_duration", "10"], min_snr_celt: 0.0 },
        Case {
            name: "music_stereo_96k_lowdelay_2_5ms",
            channels: 2,
            signal: common::music,
            args: &["-b:a", "96k", "-application", "lowdelay", "-frame_duration", "2.5"],
            min_snr_celt: 50.0,
        },
        Case {
            name: "sweep_stereo_128k_lowdelay_5ms",
            channels: 2,
            signal: sweep_sig,
            args: &["-b:a", "128k", "-application", "lowdelay", "-frame_duration", "5"],
            min_snr_celt: 50.0,
        },
        Case { name: "music_stereo_160k_cbr", channels: 2, signal: common::music, args: &["-b:a", "160k", "-vbr", "off"], min_snr_celt: 50.0 },
        Case { name: "music_mono_256k_20ms", channels: 1, signal: common::music, args: &["-b:a", "256k"], min_snr_celt: 50.0 },
        Case { name: "sweep_stereo_510k", channels: 2, signal: sweep_sig, args: &["-b:a", "510k"], min_snr_celt: 50.0 },
        Case {
            name: "music_stereo_48k_constrained",
            channels: 2,
            signal: common::music,
            args: &["-b:a", "48k", "-vbr", "constrained", "-frame_duration", "40"],
            min_snr_celt: 0.0,
        },
        Case { name: "music_51_256k", channels: 6, signal: common::music, args: &["-b:a", "256k", "-mapping_family", "1"], min_snr_celt: 0.0 },
        Case {
            name: "speech_51_96k",
            channels: 6,
            signal: common::speech,
            args: &["-b:a", "96k", "-mapping_family", "1", "-application", "lowdelay"],
            min_snr_celt: 50.0,
        },
    ];
    let mut failures = Vec::new();
    for case in &cases {
        let (snr, q, modes) = run(case, 48000);
        let celt_only = modes.iter().all(|m| *m == Mode::CeltOnly);
        let ok = if celt_only { snr >= case.min_snr_celt.max(50.0) } else { snr >= 10.0 && (q.is_nan() || q >= 0.0) };
        eprintln!("{:36} modes {:?}: snr {snr:6.1} dB quality {q:6.1} {}", case.name, modes, if ok { "ok" } else { "FAIL" });
        if !ok {
            failures.push(case.name);
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// Decoding at 8/12/16/24 kHz against libopus at 48 kHz resampled by ffmpeg. The comparison
/// includes ffmpeg's resampler, so only coarse agreement (quality metric >= 0) is required.
#[test]
fn libopus_oracle_output_rates() {
    let Some(ff) = common::ffmpeg() else {
        return;
    };
    if !has_libopus(&ff) {
        return;
    }
    let cases = [
        Case { name: "rates_music_stereo_64k", channels: 2, signal: common::music, args: &["-b:a", "64k"], min_snr_celt: 0.0 },
        Case { name: "rates_speech_mono_16k", channels: 1, signal: common::speech, args: &["-b:a", "16k", "-application", "voip"], min_snr_celt: 0.0 },
    ];
    let mut failures = Vec::new();
    for case in &cases {
        for rate in [8000u32, 12000, 16000, 24000] {
            let (snr, q, modes) = run(case, rate);
            let ok = q >= 0.0;
            eprintln!("{:28} @{rate:5} modes {:?}: snr {snr:6.1} dB quality {q:6.1} {}", case.name, modes, if ok { "ok" } else { "FAIL" });
            if !ok {
                failures.push(format!("{}@{rate}", case.name));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}
