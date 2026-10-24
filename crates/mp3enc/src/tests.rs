use super::*;
use crate::filterbank::Analysis;
use crate::tables::{alias_coefficients, synthesis_window};

/// The decoder side of the hybrid filterbank (§2.4.3.4.10: alias butterflies, IMDCT with the
/// normal window and overlap-add, frequency inversion, polyphase synthesis), for checking the
/// analysis side.
struct Synth {
    overlap: [[f32; 18]; 32],
    v: [f32; 1024],
}

impl Synth {
    fn new() -> Synth {
        Synth { overlap: [[0.0; 18]; 32], v: [0.0; 1024] }
    }

    fn granule(&mut self, xr: &[f32; GRANULE]) -> Vec<f32> {
        let mut x = *xr;
        for sb in 1..32 {
            for (i, (cs, ca)) in alias_coefficients().iter().enumerate() {
                let (up, dn) = (18 * sb - 1 - i, 18 * sb + i);
                let (bu, bd) = (x[up], x[dn]);
                x[up] = bu * cs - bd * ca;
                x[dn] = bd * cs + bu * ca;
            }
        }
        let mut sub = [[0f32; 18]; 32];
        for sb in 0..32 {
            let mut y = [0f32; 36];
            for (i, yi) in y.iter_mut().enumerate() {
                let w = (std::f64::consts::PI / 36.0 * (i as f64 + 0.5)).sin();
                let mut acc = 0f64;
                for k in 0..18 {
                    acc += f64::from(x[18 * sb + k]) * (std::f64::consts::PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos();
                }
                *yi = (acc * w) as f32;
            }
            for t in 0..18 {
                let mut v = y[t] + self.overlap[sb][t];
                if sb % 2 == 1 && t % 2 == 1 {
                    v = -v;
                }
                sub[sb][t] = v;
                self.overlap[sb][t] = y[t + 18];
            }
        }
        let d = synthesis_window();
        let mut out = Vec::with_capacity(GRANULE);
        for t in 0..18 {
            self.v.copy_within(0..960, 64);
            for i in 0..64 {
                self.v[i] = (0..32).map(|k| (((16 + i) * (2 * k + 1)) as f64 * std::f64::consts::PI / 64.0).cos() as f32 * sub[k][t]).sum();
            }
            for j in 0..32 {
                let mut acc = 0f32;
                for i in 0..8 {
                    acc += self.v[i * 128 + j] * d[i * 64 + j] + self.v[i * 128 + 96 + j] * d[i * 64 + 32 + j];
                }
                out.push(acc);
            }
        }
        out
    }
}

fn noise(n: usize, amp: f32, seed: u64) -> Vec<f32> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

fn sine(n: usize, rate: f32, hz: f32, amp: f32) -> Vec<f32> {
    (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / rate).sin()).collect()
}

fn analyse(pcm: &[f32]) -> Vec<[f32; GRANULE]> {
    let mut a = Analysis::new();
    pcm.as_chunks::<GRANULE>()
        .0
        .iter()
        .map(|c| {
            let mut x = [0f32; GRANULE];
            a.granule(c, &mut x);
            x
        })
        .collect()
}

#[test]
fn hybrid_filterbank_reconstructs_with_the_documented_delay() {
    let input = noise(GRANULE * 12, 0.5, 3);
    let mut s = Synth::new();
    let out: Vec<f32> = analyse(&input).iter().flat_map(|x| s.granule(x)).collect();
    let d = DELAY as usize;
    let err = (0..input.len() - d).map(|i| (out[i + d] - input[i]).abs()).fold(0f32, f32::max);
    assert!(err < 2e-4, "max reconstruction error {err} at delay {d}");
}

#[test]
fn full_scale_tone_energy_matches_the_threshold_scale() {
    // the MDCT is not shift invariant: average a few granules
    let x = analyse(&sine(GRANULE * 10, 44_100.0, 1000.0, 1.0));
    let e = x[2..].iter().flatten().map(|v| v * v).sum::<f32>() / 8.0;
    assert!((e / FULL_SCALE_TONE_ENERGY - 1.0).abs() < 0.1, "tone energy {e}");
}

/// Frames (MPEG-1, 44.1 kHz mono, 320 kbit/s, no reservoir) carrying the given granules.
fn frames_of(granules: &[Granule]) -> Vec<u8> {
    let mut enc = Encoder::new(Config::cbr(44_100, 1, 320)).unwrap();
    let mut out = Vec::new();
    for pair in granules.chunks(2) {
        let mut coded = pair.to_vec();
        if coded.len() == 1 {
            coded.push(enc.model.encode(&[0.0; GRANULE], &[f32::MAX; 22], 0, 0));
        }
        let len = enc.next_frame_len();
        let slot = len - 4 - enc.side_info_len();
        let mut md = BitWriter::new();
        coded.iter().for_each(|g| g.write(&mut md, &enc.model));
        let mut main = md.finish();
        assert!(main.len() <= slot, "{} main-data bytes in a {slot}-byte slot", main.len());
        main.resize(slot, 0);
        out.extend(enc.header_and_side_info(len, false, 0, &coded));
        out.extend(main);
    }
    out
}

/// A granule coding `pairs` with table `table` in all three regions, then `quads` (magnitudes
/// ≤ 1) with count1 table `c1`; every third nonzero line negative.
fn forced_granule(model: &Model, pairs: &[(u16, u16)], quads: &[[u16; 4]], table: usize, c1: usize, gain: u32) -> Granule {
    let mut g = model.encode(&[0.0; GRANULE], &[f32::MAX; 22], 0, 0);
    for (k, (x, y)) in pairs.iter().enumerate() {
        g.ix[2 * k] = *x;
        g.ix[2 * k + 1] = *y;
    }
    for (k, q) in quads.iter().enumerate() {
        g.ix[2 * pairs.len() + 4 * k..2 * pairs.len() + 4 * k + 4].copy_from_slice(q);
    }
    for (i, n) in g.negative.iter_mut().enumerate() {
        *n = i % 3 == 1;
    }
    g.global_gain = gain;
    g.coding = huffman::Coding {
        big_values: pairs.len(),
        count1: quads.len(),
        table_select: [table; 3],
        region0_count: 7,
        region1_count: 7,
        count1_table: c1,
        bits: 0,
    };
    let mut w = BitWriter::new();
    huffman::write(&mut w, &g.ix, &g.negative, &g.coding, &model.bands);
    g.coding.bits = w.bit_len();
    assert!(g.bits() <= 4095, "table {table}: {} bits", g.bits());
    g
}

/// What a standard decoder outputs for the granules (mono, no scalefactors).
fn reference_decode(granules: &[Granule]) -> Vec<f32> {
    let mut s = Synth::new();
    let p43 = crate::tables::pow43();
    let mut out = Vec::new();
    for g in granules {
        let step = 2f32.powf(0.25 * (g.global_gain as f32 - 210.0));
        let mut xr = [0f32; GRANULE];
        for (i, x) in xr.iter_mut().enumerate() {
            let v = p43[usize::from(g.ix[i])] * step;
            *x = if g.negative[i] { -v } else { v };
        }
        out.extend(s.granule(&xr));
    }
    out
}

fn ffmpeg_decode_mono(bytes: &[u8], name: &str) -> Option<(Vec<f32>, String)> {
    let ff = filmcraft_testkit::ffmpeg_or_skip("mp3enc Huffman tables")?;
    let path = filmcraft_testkit::fixtures_dir("mp3enc").join(name);
    std::fs::write(&path, bytes).unwrap();
    let out = std::process::Command::new(ff).args(["-v", "error", "-i"]).arg(&path).args(["-f", "f32le", "-acodec", "pcm_f32le", "-"]).output().unwrap();
    let pcm = out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    Some((pcm, String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// Every code of every pair table (escapes with several linbits values) and of both count1
/// tables, as ffmpeg decodes them, against the reference decode of the intended lines.
#[test]
fn every_huffman_code_decodes_as_intended_in_ffmpeg() {
    let enc = Encoder::new(Config::cbr(44_100, 1, 320)).unwrap();
    let mut granules = Vec::new();
    let mut owners = Vec::new();
    for table in (1..32).filter(|t| *t != 4 && *t != 14) {
        let size = huffman_tables::pair_table(table).unwrap().size as u16;
        let linbits = huffman_tables::LINBITS[table];
        // every code, with small escape values so that any decoding error stands out ...
        let esc = |v: u16, k: u16, big: bool| match (linbits > 0 && v == 15, big) {
            (false, _) => v,
            (true, false) => 15 + k % 8 % (1 << linbits),
            (true, true) => 15 + ((1u32 << linbits) - 1).saturating_sub(u32::from(k % 3)) as u16,
        };
        let mut pairs = Vec::new();
        for x in 0..size {
            for y in 0..size {
                pairs.push((esc(x, x + 3 * y, false), esc(y, 7 * x + y, false)));
            }
        }
        for chunk in pairs.chunks(48) {
            granules.push(forced_granule(&enc.model, chunk, &[], table, 0, 190));
            owners.push(format!("table {table}"));
        }
        // ... and the largest escapes
        if linbits > 0 {
            let big: Vec<(u16, u16)> = (0..16).map(|k| (esc(15, k, true), esc(if k % 2 == 0 { 15 } else { k }, k + 1, true))).collect();
            granules.push(forced_granule(&enc.model, &big, &[], table, 0, 120));
            owners.push(format!("table {table}, large escapes"));
        }
    }
    let quads: Vec<[u16; 4]> = (0..16u16).map(|q| [q >> 3, (q >> 2) & 1, (q >> 1) & 1, q & 1]).collect();
    for c1 in 0..2 {
        granules.push(forced_granule(&enc.model, &[(1, 2), (0, 1)], &quads, 2, c1, 190));
        owners.push(format!("count1 table {}", if c1 == 0 { "A" } else { "B" }));
    }
    let bytes = frames_of(&granules);
    let Some((ff, err)) = ffmpeg_decode_mono(&bytes, "huffman_tables.mp3") else { return };
    assert!(err.trim().is_empty(), "ffmpeg: {err}");
    let ours = reference_decode(&granules);
    assert!(ff.len() >= ours.len(), "{} vs {}", ff.len(), ours.len());
    // a wrong code desynchronises the rest of its granule: report the first granule that differs
    for (k, owner) in owners.iter().enumerate() {
        let r = k * GRANULE..(k + 1) * GRANULE;
        let peak = ours[r.clone()].iter().fold(1e-3f32, |m, v| m.max(v.abs()));
        let diff = ours[r.clone()].iter().zip(&ff[r]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(diff < 1e-5 * peak, "{owner} (granule {k}): differs by {diff} (peak {peak})");
    }
}

/// A frame's header and side-information fields we check.
#[derive(Debug)]
struct Parsed {
    lsf: bool,
    bitrate_index: u32,
    rate_index: u32,
    padding: bool,
    mode: u32,
    mode_extension: u32,
    len: usize,
    main_data_begin: usize,
    part2_3: Vec<usize>,
    global_gain: Vec<u32>,
}

/// Walk the frames of a stream, checking sync and lengths.
fn parse(bytes: &[u8]) -> Vec<Parsed> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let h = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap());
        assert_eq!(h >> 21, 0x7FF, "sync at {pos}");
        let lsf = (h >> 19) & 3 == 2;
        assert!(lsf || (h >> 19) & 3 == 3, "MPEG-1 or MPEG-2");
        assert_eq!((h >> 17) & 3, 1, "Layer III");
        assert_eq!((h >> 16) & 1, 1, "no CRC");
        let bitrate_index = (h >> 12) & 15;
        let rate_index = (h >> 10) & 3;
        let padding = (h >> 9) & 1 == 1;
        let mode = (h >> 6) & 3;
        let kbps = if lsf { tables::BITRATES_LSF } else { tables::BITRATES_MPEG1 }[bitrate_index as usize - 1];
        let rate = if lsf { tables::RATES_LSF } else { tables::RATES_MPEG1 }[rate_index as usize];
        let len = (if lsf { 72_000 } else { 144_000 } * kbps / rate) as usize + usize::from(padding);
        let nch = if mode == 3 { 1 } else { 2 };
        let mut r = filmcraft_bitstream::BitReader::new(&bytes[pos + 4..pos + len]);
        let main_data_begin = r.read_bits(if lsf { 8 } else { 9 }).unwrap() as usize;
        r.skip(if lsf { nch } else { [5, 3][nch - 1] + 4 * nch }).unwrap();
        let (mut part2_3, mut global_gain) = (Vec::new(), Vec::new());
        for _ in 0..(if lsf { 1 } else { 2 }) * nch {
            part2_3.push(r.read_bits(12).unwrap() as usize);
            let big_values = r.read_bits(9).unwrap();
            assert!(big_values <= 288);
            global_gain.push(r.read_bits(8).unwrap());
            r.skip(if lsf { 9 } else { 4 }).unwrap();
            assert!(!r.read_flag().unwrap(), "long blocks only");
            r.skip(15 + 7 + if lsf { 2 } else { 3 }).unwrap();
        }
        out.push(Parsed { lsf, bitrate_index, rate_index, padding, mode, mode_extension: (h >> 4) & 3, len, main_data_begin, part2_3, global_gain });
        pos += len;
    }
    assert_eq!(pos, bytes.len(), "whole frames");
    out
}

fn encode_all(cfg: Config, input: &[Vec<f32>]) -> Vec<u8> {
    let mut enc = Encoder::new(cfg).unwrap();
    let planes: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
    let mut out = enc.encode(&planes);
    out.extend(enc.flush());
    out
}

#[test]
fn headers_carry_the_configuration_and_frames_have_their_nominal_sizes() {
    let n = 44_100;
    let input = vec![sine(n, 44_100.0, 440.0, 0.5), sine(n, 44_100.0, 660.0, 0.4)];
    let frames = parse(&encode_all(Config::cbr(44_100, 2, 128), &input));
    // the decoder needs (input + delay) samples: whole frames of 1152
    assert_eq!(frames.len(), (n + DELAY as usize).div_ceil(1152));
    let f = &frames[0];
    assert_eq!((f.lsf, f.bitrate_index, f.rate_index, f.mode), (false, 9, 0, 1));
    // 44.1 kHz: 417 or 418 bytes, padded so that the mean is exactly 128 kbit/s
    let total: usize = frames.iter().map(|f| f.len).sum();
    let exact = 144_000.0 * 128.0 / 44_100.0 * frames.len() as f64;
    assert!((total as f64 - exact).abs() < 1.0, "{total} vs {exact}");
    assert!(frames.iter().any(|f| f.padding) && frames.iter().any(|f| !f.padding));
    // the same tone in both channels at nearly the same level: mid/side
    let ms = parse(&encode_all(Config::cbr(44_100, 2, 128), &[input[0].clone(), input[0].iter().map(|v| v * 0.9).collect()]));
    assert!(ms.iter().skip(1).all(|f| f.mode_extension == 2), "mid/side expected");
    // simple stereo never uses mid/side
    let st = parse(&encode_all(Config { stereo: StereoMode::Stereo, ..Config::cbr(48_000, 2, 192) }, &input));
    assert!(st.iter().all(|f| f.mode == 0 && f.mode_extension == 0));
    // MPEG-2 LSF mono
    let lsf = parse(&encode_all(Config::cbr(22_050, 1, 32), &input[..1]));
    assert_eq!((lsf[0].lsf, lsf[0].rate_index, lsf[0].mode, lsf[0].part2_3.len()), (true, 0, 3, 1));
    assert_eq!(lsf.len(), (n + DELAY as usize).div_ceil(576));
}

#[test]
fn the_reservoir_lends_bits_within_the_decoder_buffer() {
    // quiet passages save bits that a loud burst of noise then borrows
    let mut x = vec![0f32; 44_100];
    let burst = noise(4410, 0.7, 9);
    x[22_050..26_460].copy_from_slice(&burst);
    x.iter_mut().take(22_050).enumerate().for_each(|(i, v)| *v = 0.01 * (i as f32 * 0.05).sin());
    for (kbps, max_begin) in [(64u32, 511usize), (128, 511), (256, 125), (320, 0)] {
        let frames = parse(&encode_all(Config::cbr(44_100, 1, kbps), &[x.clone()]));
        let mut slots = 0usize;
        let mut lent = 0;
        for f in &frames {
            let slot = f.len - 4 - 17;
            assert!(f.main_data_begin <= max_begin, "{kbps} kbps: main_data_begin {}", f.main_data_begin);
            assert!(f.main_data_begin <= slots, "{kbps} kbps: points before the stream");
            let bits: usize = f.part2_3.iter().sum();
            assert!(bits <= 8 * (f.main_data_begin + slot), "{kbps} kbps: {bits} bits of main data");
            assert!(f.part2_3.iter().all(|&b| b <= 4095));
            slots += slot;
            lent = lent.max(f.main_data_begin);
        }
        if max_begin > 0 {
            assert!(lent > 0, "{kbps} kbps: the reservoir was never used");
        }
    }
}

#[test]
fn hostile_settings_are_refused() {
    for rate in [0, 8_000, 11_025, 44_101, 96_000, u32::MAX] {
        assert_eq!(Encoder::new(Config::cbr(rate, 2, 128)).err(), Some(Error::SampleRate(rate)));
    }
    for kbps in [0, 8, 33, 144, 384, u32::MAX] {
        assert!(matches!(Encoder::new(Config::cbr(44_100, 2, kbps)), Err(Error::Bitrate { .. })), "{kbps}");
    }
    assert!(Encoder::new(Config::cbr(24_000, 1, 8)).is_ok());
    assert!(matches!(Encoder::new(Config::cbr(24_000, 1, 192)), Err(Error::Bitrate { .. })));
    for ch in [0, 3, 6, usize::MAX] {
        assert_eq!(Encoder::new(Config::cbr(48_000, ch, 128)).err(), Some(Error::Channels(ch)));
    }
    assert_eq!(nearest_bitrate(44_100, 1000), Some(320));
    assert_eq!(nearest_bitrate(44_100, 130), Some(128));
    assert_eq!(nearest_bitrate(44_100, 0), Some(32));
    assert_eq!(nearest_bitrate(22_050, 320), Some(160));
    assert_eq!(nearest_bitrate(96_000, 128), None);
    assert_eq!(
        (nearest_sample_rate(44_100), nearest_sample_rate(96_000), nearest_sample_rate(8_000), nearest_sample_rate(0)),
        (44_100, 48_000, 16_000, 16_000)
    );
    assert_eq!(nearest_sample_rate(u32::MAX), 48_000);
    assert!(bitrates(0).is_empty());
}

#[test]
fn hostile_samples_still_make_valid_frames() {
    let mut enc = Encoder::new(Config::cbr(48_000, 2, 96)).unwrap();
    let mut out = Vec::new();
    let wild: Vec<f32> = (0..5000)
        .map(|i| match i % 5 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => -1e30,
            3 => 1e-40,
            _ => 3.0,
        })
        .collect();
    out.extend(enc.encode(&[&wild, &wild[..10]])); // ragged: the second channel is padded with silence
    out.extend(enc.encode(&[&wild[..7]])); // a missing channel is silence
    out.extend(enc.encode(&[]));
    out.extend(enc.encode(&[&[], &[], &[]]));
    out.extend(enc.flush());
    out.extend(enc.flush()); // a second flush adds nothing
    let frames = parse(&out);
    assert_eq!(frames.len(), (5007 + DELAY as usize).div_ceil(1152));
    assert!(frames.iter().all(|f| f.global_gain.iter().all(|&g| g <= 255)));
    // no input, no frames
    let mut empty = Encoder::new(Config::cbr(44_100, 1, 64)).unwrap();
    assert!(empty.flush().is_empty());
}

#[test]
fn chunking_does_not_change_the_stream() {
    let input = vec![sine(20_000, 32_000.0, 300.0, 0.3), noise(20_000, 0.1, 5)];
    let whole = encode_all(Config::cbr(32_000, 2, 96), &input);
    let mut enc = Encoder::new(Config::cbr(32_000, 2, 96)).unwrap();
    let mut parts = Vec::new();
    for k in (0..20_000).step_by(777) {
        let e = (k + 777).min(20_000);
        parts.extend(enc.encode(&[&input[0][k..e], &input[1][k..e]]));
    }
    parts.extend(enc.flush());
    assert!(whole == parts, "chunked encode differs");
}
