//! The encoder's decisions are pinned: the coded slices of synthetic clips hash to fixed values,
//! so a change that alters any decision (a mode, a motion vector, a quantiser), and not only one
//! that breaks decoding, fails here. Only slice NAL units are hashed: the SEI names the version.

use crate::common::{annexb_nals, synth};
use filmcraft_h264enc::*;

/// FNV-1a over the slice NAL units of `n` frames of [`synth`] encoded with `c`.
fn slices_hash(c: EncoderConfig, n: usize) -> u64 {
    let (w, h) = (c.width as usize, c.height as usize);
    let mut enc = Encoder::new(c).unwrap();
    let mut stream = Vec::new();
    for t in 0..n {
        let f = synth(w, h, t);
        for p in enc.encode(&f.frame(), t as i64).unwrap() {
            stream.extend(p.data);
        }
    }
    for p in enc.flush() {
        stream.extend(p.data);
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for r in annexb_nals(&stream) {
        let nal = &stream[r];
        if matches!(nal.get(3).map(|b| b & 31), Some(1 | 5)) {
            for &b in nal {
                hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    hash
}

fn cfg(profile: Profile, preset: Preset, rate: RateControl) -> EncoderConfig {
    let mut c = EncoderConfig::new(320, 192, 24, 1);
    c.profile = profile;
    c.preset = preset;
    c.rate = rate;
    c.slices = 3;
    c.threads = 3;
    c
}

#[test]
fn coded_slices_are_pinned() {
    let cases = [
        ("high balanced crf", cfg(Profile::High, Preset::Balanced, RateControl::Crf(23.0)), 0xecd1_35a9_3a46_f7d8u64),
        ("high quality vbr", cfg(Profile::High, Preset::Quality, RateControl::Vbr { target_kbps: 800, max_kbps: 1200 }), 0x251e_c302_fd8a_ed38),
        ("main speed qp", cfg(Profile::Main, Preset::Speed, RateControl::Qp(30)), 0xa123_355d_d580_4e7b),
        ("baseline balanced cbr", cfg(Profile::Baseline, Preset::Balanced, RateControl::Cbr { kbps: 600 }), 0x13cb_e982_aee0_13f8),
    ];
    let got: Vec<u64> = cases.iter().map(|(_, c, _)| slices_hash(c.clone(), 20)).collect();
    for ((name, _, _), got) in cases.iter().zip(&got) {
        eprintln!("{name}: {got:#018x}");
    }
    for ((name, _, want), got) in cases.iter().zip(&got) {
        assert_eq!(*got, *want, "{name}: the coded slices changed");
    }
}
