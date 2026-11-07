//! Encoder round trips through our own decoder: every packet decodes, the length after the
//! pre-skip covers the input, and the SNR against the input meets per-case floors.

use filmcraft_opus::{Decoder, Encoder, EncoderConfig, FRAME_SIZE, PRE_SKIP};

use crate::common;

fn tone(n: usize, f: f32, amp: f32) -> Vec<f32> {
    (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / 48000.0).sin()).collect()
}

fn chords(n: usize) -> Vec<f32> {
    let a = tone(n, 220.0, 0.25);
    let b = tone(n, 277.18, 0.2);
    let c = tone(n, 329.63, 0.15);
    let d = tone(n, 1760.0, 0.05);
    (0..n).map(|i| a[i] + b[i] + c[i] + d[i]).collect()
}

fn noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            amp * ((s >> 8) as f32 / 8_388_608.0 - 1.0)
        })
        .collect()
}

/// Encodes `input` (planar), decodes every packet, drops the pre-skip; returns the decoded
/// planar output and the packet sizes.
pub fn round_trip(input: &[Vec<f32>], bitrate: u32) -> (Vec<Vec<f32>>, Vec<usize>) {
    let ch = input.len();
    let mut enc = Encoder::new(EncoderConfig::new(ch, bitrate)).unwrap();
    let mut packets = Vec::new();
    // Uneven chunks: buffering must not change the stream.
    let n = input[0].len();
    let mut at = 0;
    let mut step = 333;
    while at < n {
        let end = (at + step).min(n);
        let slices: Vec<&[f32]> = input.iter().map(|c| &c[at..end]).collect();
        packets.extend(enc.encode(&slices).unwrap());
        at = end;
        step = step * 7 % 2011 + 1;
    }
    packets.extend(enc.finish().unwrap());
    let mut dec = Decoder::simple(48000, ch).unwrap();
    let mut out = vec![Vec::new(); ch];
    let mut sizes = Vec::new();
    for p in &packets {
        sizes.push(p.len());
        let pcm = dec.decode(Some(p)).unwrap();
        for (o, c) in out.iter_mut().zip(pcm) {
            o.extend(c);
        }
    }
    assert_eq!(packets.len() as u64, (n as u64 + u64::from(PRE_SKIP)).div_ceil(FRAME_SIZE as u64));
    for o in out.iter_mut() {
        o.drain(..PRE_SKIP as usize);
        assert!(o.len() >= n, "decoded {} samples for {n}", o.len());
        o.truncate(n);
    }
    (out, sizes)
}

fn snr(input: &[Vec<f32>], out: &[Vec<f32>]) -> f64 {
    // skip the first frame: the encoder starts from silence
    let skip = 2 * FRAME_SIZE;
    input.iter().zip(out).map(|(a, b)| common::snr_db(&a[skip..], &b[skip..])).fold(f64::MAX, f64::min)
}

/// SNR floors about 6 dB under what the encoder measured when written (waveform SNR understates
/// a perceptual codec; the point is to catch regressions, not to rate quality).
#[test]
fn snr_meets_floors() {
    let n = 48000 * 2;
    let chords_st = vec![chords(n), chords(n).iter().map(|v| v * 0.6).collect()];
    let cases: Vec<(&str, Vec<Vec<f32>>, &[(u32, f64)])> = vec![
        ("chords mono", vec![chords(n)], &[(16_000, 7.0), (32_000, 10.0), (64_000, 16.0), (128_000, 20.0), (256_000, 40.0)]),
        ("music mono", vec![common::music(n, 5)], &[(64_000, 14.0), (128_000, 19.0), (256_000, 26.0)]),
        ("tone mono", vec![tone(n, 997.0, 0.5)], &[(128_000, 22.0), (510_000, 41.0)]),
        ("chords stereo", chords_st, &[(16_000, 7.0), (32_000, 17.0), (64_000, 22.0), (128_000, 26.0), (256_000, 31.0), (510_000, 40.0)]),
    ];
    for (name, input, floors) in &cases {
        for &(br, floor) in floors.iter() {
            let (out, sizes) = round_trip(input, br);
            assert!(sizes.iter().all(|&s| s == (br as usize + 200) / 400), "{name}: constant packet size");
            let got = snr(input, &out);
            assert!(got >= floor, "{name} at {br} bit/s: {got:.2} dB < {floor} dB");
        }
    }
}

#[test]
fn every_bit_rate_decodes() {
    let n = 48000 / 2;
    let input = [common::music(n, 1), noise(n, 2, 0.7)];
    for br in [0u32, 1200, 6000, 8000, 12_345, 24_000, 96_000, 300_000, 510_400, u32::MAX] {
        for ch in 1..=2 {
            let (out, sizes) = round_trip(&input[..ch], br);
            assert!((3..=1276).contains(&sizes[0]), "{br}: {}", sizes[0]);
            // Noise peaking at 0.7 decodes with peaks near 1.6 at every rate: aggressive spreading
            // keeps low-rate bands from collapsing into a few spikes.
            let peak = out.iter().flatten().fold(0f32, |m, v| if v.is_finite() { m.max(v.abs()) } else { f32::INFINITY });
            assert!(peak < 3.0, "{br} bit/s, {ch} ch: peak {peak}");
        }
    }
}

#[test]
fn silence_stays_silent_and_output_is_deterministic() {
    let n = 48000;
    let (out, _) = round_trip(&[vec![0.0; n], vec![0.0; n]], 96_000);
    assert!(out.iter().flatten().all(|v| v.abs() < 1e-3), "silence");
    let input = vec![common::music(n, 3)];
    let a = round_trip(&input, 64_000).0;
    let b = round_trip(&input, 64_000).0;
    assert_eq!(a, b);
}

#[test]
fn the_end_of_the_input_is_decoded() {
    // A click in the very last sample survives the padding.
    for n in [1usize, 959, 960, 961, 48_000] {
        let mut x = vec![0f32; n];
        x[n - 1] = 0.9;
        let (out, _) = round_trip(&[x], 256_000);
        assert_eq!(out[0].len(), n);
        let tail = &out[0][n.saturating_sub(64)..];
        assert!(tail.iter().any(|v| v.abs() > 0.05), "n={n}: the final click is missing");
    }
}

#[test]
fn hostile_input_is_an_error_not_a_panic() {
    assert!(Encoder::new(EncoderConfig::new(0, 64_000)).is_err());
    assert!(Encoder::new(EncoderConfig::new(3, 64_000)).is_err());
    let mut e = Encoder::new(EncoderConfig::new(2, 64_000)).unwrap();
    assert!(e.encode(&[&[0.0; 10]]).is_err(), "one channel of two");
    assert!(e.encode(&[&[0.0; 10], &[0.0; 9]]).is_err(), "ragged");
    assert!(e.encode(&[&[f32::NAN; 10], &[0.0; 10]]).is_err(), "NaN");
    assert!(e.encode(&[&[0.0; 10], &[f32::INFINITY; 10]]).is_err(), "inf");
    // Huge but finite values are clamped and still decode.
    let loud = vec![1e30f32; 4000];
    let packets = e.encode(&[&loud, &loud]).unwrap();
    let mut dec = Decoder::simple(48000, 2).unwrap();
    for p in &packets {
        assert!(dec.decode(Some(p)).unwrap().iter().flatten().all(|v| v.is_finite()));
    }
    assert!(!e.finish().unwrap().is_empty());
    assert!(e.finish().unwrap().is_empty(), "finish twice");
    assert!(e.encode(&[&[0.0; 10], &[0.0; 10]]).is_err(), "encode after finish");
    // Nothing in, one packet out (the pre-skip).
    let mut e = Encoder::new(EncoderConfig::new(1, 64_000)).unwrap();
    assert_eq!(e.finish().unwrap().len(), 1);
}

#[test]
fn head_describes_the_stream() {
    let e = Encoder::new(EncoderConfig::new(2, 160_000)).unwrap();
    let h = e.head();
    assert_eq!((h.channels, h.pre_skip, h.input_sample_rate, h.mapping_family), (2, PRE_SKIP, 48_000, 0));
    assert_eq!(filmcraft_opus::OpusHead::parse(&h.to_bytes()).unwrap(), h);
    assert_eq!(e.bitrate(), 160_000);
    assert_eq!(e.packet_bytes(), 400);
}
