//! Decoder accuracy against ffmpeg's decoder on streams produced by ffmpeg's AAC encoder (oracle
//! only; skipped when ffmpeg is absent).

use crate::common;

use common::*;

struct Case {
    name: &'static str,
    rate: u32,
    channels: usize,
    args: &'static [&'static str],
}

fn input(rate: u32, channels: usize) -> Vec<Vec<f32>> {
    let n = rate as usize * 3 / 2;
    (0..channels)
        .map(|c| {
            let a = chords(rate, n, 0.003 * c as f32);
            let b = clicks(rate, n);
            let s = speech(rate, n, 5 + c as u64);
            a.iter().zip(&b).zip(&s).map(|((x, y), z)| 0.6 * x + 0.3 * y + 0.3 * z * if c % 2 == 0 { 1.0 } else { -0.5 }).collect()
        })
        .collect()
}

/// Returns (max abs error, samples compared).
fn compare(case: &Case) -> Option<(f32, usize)> {
    let ff = ffmpeg()?;
    let path = fixtures().join(format!("ffenc_{}.aac", case.name));
    ffmpeg_encode(&ff, &input(case.rate, case.channels), case.rate, case.args, &path);
    let data = std::fs::read(&path).unwrap();
    let (ref_pcm, err) = ffmpeg_decode(&ff, &path, case.channels);
    assert!(err.trim().is_empty(), "{err}");
    let ours = our_decode(&data);
    assert_eq!(ours.len(), case.channels);
    // ffmpeg reorders channels to its native layouts; match each of our (AAC element order)
    // channels to the closest ffmpeg channel and require a permutation.
    let mut worst = 0f32;
    let mut taken = vec![false; case.channels];
    for c in 0..case.channels {
        let (best, err) =
            (0..case.channels).filter(|&j| !taken[j]).map(|j| (j, max_abs_diff(&ours[c], &ref_pcm[j]))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        taken[best] = true;
        assert_eq!(ours[c].len(), ref_pcm[best].len(), "{}: length", case.name);
        worst = worst.max(err);
    }
    Some((worst, ours[0].len()))
}

#[test]
fn matches_ffmpeg_decoder() {
    if ffmpeg().is_none() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let cases = [
        Case { name: "stereo_44k_128k", rate: 44100, channels: 2, args: &["-b:a", "128k", "-aac_pns", "0"] },
        Case { name: "stereo_48k_256k", rate: 48000, channels: 2, args: &["-b:a", "256k", "-aac_pns", "0"] },
        Case { name: "mono_48k_64k", rate: 48000, channels: 1, args: &["-b:a", "64k", "-aac_pns", "0"] },
        Case {
            name: "stereo_44k_96k_is_ms_tns",
            rate: 44100,
            channels: 2,
            args: &["-b:a", "96k", "-aac_pns", "0", "-aac_is", "1", "-aac_ms", "1", "-aac_tns", "1"],
        },
        Case { name: "stereo_32k_48k_is", rate: 32000, channels: 2, args: &["-b:a", "48k", "-aac_pns", "0", "-aac_is", "1"] },
        Case { name: "mono_22k_32k", rate: 22050, channels: 1, args: &["-b:a", "32k", "-aac_pns", "0"] },
        Case { name: "mono_16k_24k", rate: 16000, channels: 1, args: &["-b:a", "24k", "-aac_pns", "0"] },
        Case { name: "mono_8k_16k", rate: 8000, channels: 1, args: &["-b:a", "16k", "-aac_pns", "0"] },
        Case { name: "stereo_96k_256k", rate: 96000, channels: 2, args: &["-b:a", "256k", "-aac_pns", "0"] },
        Case { name: "c3_48k", rate: 48000, channels: 3, args: &["-b:a", "192k", "-aac_pns", "0"] },
        Case { name: "c51_48k_320k", rate: 48000, channels: 6, args: &["-b:a", "320k", "-aac_pns", "0"] },
        Case { name: "c71_48k_448k", rate: 48000, channels: 8, args: &["-b:a", "448k", "-aac_pns", "0"] },
        Case { name: "stereo_44k_fast_coder", rate: 44100, channels: 2, args: &["-b:a", "160k", "-aac_coder", "fast", "-aac_pns", "0"] },
    ];
    println!("{:<28} {:>10} {:>12}", "stream", "samples/ch", "max |err|");
    let mut fails = Vec::new();
    for c in &cases {
        let (err, n) = compare(c).unwrap();
        println!("{:<28} {:>10} {:>12.3e}", c.name, n, err);
        if err >= 1e-3 {
            fails.push((c.name, err));
        }
    }
    assert!(fails.is_empty(), "decoder mismatches: {fails:?}");
}

/// PNS output is random by design, and ffmpeg's encoder does not emit it: build noise-substituted
/// access units by hand (mono, and a common-window pair with M/S = correlated noise) and compare
/// per-frame energies and the L/R correlation with ffmpeg's decode.
#[test]
fn pns_matches_ffmpeg() {
    use filmcraft_aac::AdtsHeader;
    use filmcraft_bitstream::BitWriter;
    let Some(ff) = ffmpeg() else { return };
    fn ics(bw: &mut BitWriter, with_info: bool, nrg: u32) {
        bw.write_bits(100, 8); // global_gain
        if with_info {
            bw.write_bits(0, 4); // reserved, ONLY_LONG, sine
            bw.write_bits(40, 6);
            bw.write_bits(0, 1);
        }
        bw.write_bits(13, 4); // NOISE_HCB for all 40 bands
        bw.write_bits(31, 5);
        bw.write_bits(9, 5);
        bw.write_bits(nrg, 9); // first noise energy (PCM)
        for _ in 1..40 {
            bw.write_bits(0, 1); // difference 0
        }
        bw.write_bits(0, 3); // pulse, tns, gain control
    }
    for channels in [1usize, 2] {
        let mut stream = Vec::new();
        for f in 0..12u32 {
            let mut bw = BitWriter::new();
            let nrg = 256 + 20 + (f % 6) * 7;
            if channels == 1 {
                bw.write_bits(0, 3);
                bw.write_bits(0, 4);
                ics(&mut bw, true, nrg);
            } else {
                bw.write_bits(1, 3);
                bw.write_bits(0, 4);
                bw.write_bits(1, 1); // common_window
                bw.write_bits(0, 4);
                bw.write_bits(40, 6);
                bw.write_bits(0, 1);
                bw.write_bits(2, 2); // ms_mask_present = all
                ics(&mut bw, false, nrg);
                ics(&mut bw, false, nrg - 8);
            }
            bw.write_bits(7, 3);
            let au = bw.finish();
            stream.extend_from_slice(&AdtsHeader::write(2, 4, channels as u8, au.len(), 0x7FF));
            stream.extend_from_slice(&au);
        }
        let path = fixtures().join(format!("handmade_pns_{channels}.aac"));
        std::fs::write(&path, &stream).unwrap();
        let (ref_pcm, err) = ffmpeg_decode(&ff, &path, channels);
        assert!(err.trim().is_empty(), "{err}");
        let ours = our_decode(&stream);
        let mut worst_db = 0f64;
        for c in 0..channels {
            let e = |v: &[f32]| 10.0 * v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().log10();
            let (eo, ef) = (e(&ours[c]), e(&ref_pcm[c]));
            println!("PNS {channels}ch ch{c}: total energy ours {eo:.2} dB, ffmpeg {ef:.2} dB");
            assert!((eo - ef).abs() < 0.5);
            assert_eq!(ours[c].len(), ref_pcm[c].len());
            for s in (1024..ours[c].len() - 1024).step_by(1024) {
                let ea: f64 = ours[c][s..s + 1024].iter().map(|v| (*v as f64).powi(2)).sum();
                let eb: f64 = ref_pcm[c][s..s + 1024].iter().map(|v| (*v as f64).powi(2)).sum();
                worst_db = worst_db.max((10.0 * (ea / eb).log10()).abs());
            }
        }
        let corr = |a: &[f32], b: &[f32]| {
            let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
            for (x, y) in a.iter().zip(b) {
                ab += *x as f64 * *y as f64;
                aa += (*x as f64).powi(2);
                bb += (*y as f64).powi(2);
            }
            ab / (aa * bb).sqrt()
        };
        let (c_ours, c_ff) = if channels == 2 { (corr(&ours[0], &ours[1]), corr(&ref_pcm[0], &ref_pcm[1])) } else { (0.0, 0.0) };
        println!("PNS {channels}ch: per-frame energy max deviation {worst_db:.2} dB; L/R correlation ours {c_ours:.3} ffmpeg {c_ff:.3}");
        if channels == 1 {
            assert!(worst_db < 1.0);
        } else {
            // ISO/IEC 14496-3 §4.6.13: noise bands with ms_used share one random vector (correlated
            // noise). ffmpeg does not implement this and generates independent noise.
            assert!(c_ours > 0.99);
        }
    }
}
