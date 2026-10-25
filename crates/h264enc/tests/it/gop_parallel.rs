//! The GOP-parallel path (constant QP / constant quality) gives exactly the packets and the
//! reconstruction of the serial path, only later.

use crate::common;

use common::{Yuv, synth};
use filmcraft_h264enc::*;

/// Synthetic pictures with a scene cut every 13 pictures (the luma is inverted and the chroma
/// planes swapped in every other scene), so IDRs also fall inside a `keyint`.
fn scenes(w: usize, h: usize, n: usize) -> Vec<Yuv> {
    (0..n)
        .map(|i| {
            let mut f = synth(w, h, i);
            if (i / 13) % 2 == 1 {
                for v in f.y.iter_mut() {
                    *v = 255 - *v;
                }
                std::mem::swap(&mut f.u, &mut f.v);
            }
            f
        })
        .collect()
}

struct Run {
    packets: Vec<Packet>,
    recon: Vec<ReconFrame>,
    /// After every call: (packets returned so far, `packets_due`).
    calls: Vec<(u64, u64)>,
    parallel: bool,
}

/// Encode `frames` (pts 2·i), flushing after the pictures in `flush_after`, capturing the
/// reconstruction from picture `capture_from` on.
fn encode(cfg: EncoderConfig, frames: &[Yuv], flush_after: &[usize], capture_from: usize) -> Run {
    let mut enc = Encoder::new(cfg).unwrap();
    let mut packets = Vec::new();
    let mut calls = Vec::new();
    let mut recon = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        if i == capture_from {
            enc.set_recon_capture(true);
        }
        packets.extend(enc.try_encode(&f.frame(), 2 * i as i64).unwrap());
        calls.push((packets.len() as u64, enc.packets_due()));
        if flush_after.contains(&i) {
            packets.extend(enc.flush().unwrap());
            calls.push((packets.len() as u64, enc.packets_due()));
        }
        recon.extend(enc.take_recon());
    }
    packets.extend(enc.flush().unwrap());
    calls.push((packets.len() as u64, enc.packets_due()));
    recon.extend(enc.take_recon());
    Run { packets, recon, calls, parallel: enc.gop_parallel() }
}

/// Assert that both runs gave the same packets and reconstructions, that the serial run returned
/// exactly the packets due after every call, and that the parallel one never returned more and had
/// returned them all by the end.
fn assert_same(name: &str, serial: &Run, par: &Run) {
    assert_eq!(serial.packets.len(), par.packets.len(), "{name}: packet count");
    for (i, (a, b)) in serial.packets.iter().zip(&par.packets).enumerate() {
        assert!(a.data == b.data, "{name}: packet {i} bytes differ");
        assert_eq!((a.pts, a.dts, a.keyframe, a.frame_type, a.qp), (b.pts, b.dts, b.keyframe, b.frame_type, b.qp), "{name}: packet {i}");
    }
    assert_eq!(serial.recon.len(), par.recon.len(), "{name}: reconstruction count");
    for (i, (a, b)) in serial.recon.iter().zip(&par.recon).enumerate() {
        assert_eq!(a.pts, b.pts, "{name}: reconstruction {i} pts");
        assert!(a.y == b.y && a.u == b.u && a.v == b.v, "{name}: reconstruction {i} differs");
    }
    // the serial path returns exactly what is due; the parallel one never more, and all of it by the end
    for &(got, due) in &serial.calls {
        assert_eq!(got, due, "{name}: the serial path returns what is due");
    }
    let dues = |r: &Run| r.calls.iter().map(|c| c.1).collect::<Vec<_>>();
    assert_eq!(dues(serial), dues(par), "{name}: packets due");
    for &(got, due) in &par.calls {
        assert!(got <= due, "{name}: more packets than due");
    }
    assert_eq!(par.calls.last().map(|c| c.0), Some(par.packets.len() as u64));
}

/// Annex B NAL unit types of a packet (start-code prefixed, as the encoder writes them).
fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut t = Vec::new();
    let mut i = 0;
    while i + 4 < data.len() {
        if data[i..i + 4] == [0, 0, 0, 1] {
            t.push(data[i + 4] & 0x1f);
            i += 4;
        } else {
            i += 1;
        }
    }
    t
}

/// CRF and constant QP give the serial path's packets and reconstructions over 0–3 B-frames, every
/// profile, 1 and 4 slices, `keyint` 5 and 250, both packet formats, HDR SEI, flushes and late
/// reconstruction capture, with the identification SEI in the first access unit only.
#[test]
fn gop_parallel_matches_serial() {
    let (w, h, n) = (176, 144, 30);
    let frames = scenes(w, h, n);
    let mut k = 0usize;
    for rate in [RateControl::Crf(23.0), RateControl::Qp(27)] {
        for b in 0..=3u8 {
            let profiles: &[Profile] = if b == 0 { &[Profile::Baseline, Profile::High] } else { &[Profile::Main, Profile::High] };
            for &profile in profiles {
                for slices in [1, 4] {
                    for keyint in [5, 250] {
                        k += 1;
                        let mut c = EncoderConfig::new(w as u32, h as u32, 10, 1);
                        c.rate = rate;
                        c.bframes = b;
                        c.profile = profile;
                        c.slices = slices;
                        c.keyint = keyint;
                        c.threads = [2, 3, 8][k % 3];
                        c.format = if k.is_multiple_of(2) { PacketFormat::AnnexB } else { PacketFormat::LengthPrefixed };
                        if (k / 2).is_multiple_of(2) {
                            c.mastering_display = Some([9; 24]);
                            c.content_light = Some((1000, 400));
                        }
                        // flushes in the middle of a mini-GOP (picture 7 is the 8th) and of a GOP
                        let flush_after: &[usize] = match (k / 3) % 3 {
                            0 => &[],
                            1 => &[7],
                            _ => &[7, 21],
                        };
                        let capture_from = if k.is_multiple_of(5) { 9 } else { 0 };
                        let name =
                            format!("{rate:?} b{b} {profile:?} slices {slices} keyint {keyint} threads {} {:?} flush {flush_after:?}", c.threads, c.format);
                        let annexb = c.format == PacketFormat::AnnexB;
                        let par = encode(c.clone(), &frames, flush_after, capture_from);
                        c.parallel_gops = false;
                        let serial = encode(c, &frames, flush_after, capture_from);
                        assert!(par.parallel && !serial.parallel, "{name}: paths");
                        assert_same(&name, &serial, &par);
                        let idrs = serial.packets.iter().filter(|p| p.frame_type == FrameType::Idr).count();
                        // keyint 250: the scene cuts at 13 and 26 make three GOPs; keyint 5: more
                        assert!(idrs >= 3, "{name}: {idrs} IDRs");
                        // the open GOP's packets wait for the next IDR: the parallel path was taken
                        assert!(par.calls.iter().any(|&(got, due)| got < due), "{name}: no packet came late");
                        if annexb {
                            // the identification SEI goes with the first access unit only
                            for (i, p) in par.packets.iter().enumerate() {
                                assert_eq!(nal_types(&p.data).contains(&6), i == 0, "{name}: SEI in packet {i}");
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 320x240 in 15 slices with three B-frames and the default `keyint` gives the serial path's
/// output.
#[test]
fn larger_pictures_many_slices_default_keyint() {
    // 320x240: 15 macroblock rows in 15 slices, scene cuts make the GOPs
    let frames = scenes(320, 240, 30);
    for rate in [RateControl::Crf(20.5), RateControl::Qp(24)] {
        let mut c = EncoderConfig::new(320, 240, 10, 1);
        c.rate = rate;
        c.slices = 15;
        c.threads = 8;
        c.bframes = 3;
        let par = encode(c.clone(), &frames, &[], 0);
        c.parallel_gops = false;
        let serial = encode(c, &frames, &[], 0);
        assert!(par.parallel);
        assert_same(&format!("{rate:?} 320x240"), &serial, &par);
    }
}

/// CBR and VBR, in one pass and in both passes of two, take the serial path; CRF and constant QP
/// do not.
#[test]
fn bitrate_modes_take_the_serial_path() {
    let enc = |rate: RateControl, pass: Pass| {
        let mut c = EncoderConfig::new(176, 144, 10, 1);
        c.rate = rate;
        c.pass = pass;
        c.threads = 8;
        Encoder::new(c).unwrap().gop_parallel()
    };
    let stats = PassStats::from_text("h264enc-pass1 fps=10\nI 1.5 9000 400\nP 2 3000 300\n").unwrap();
    for rate in [RateControl::Cbr { kbps: 800 }, RateControl::Vbr { target_kbps: 600, max_kbps: 900 }] {
        assert!(!enc(rate, Pass::Single), "{rate:?}");
        assert!(!enc(rate, Pass::First), "{rate:?} first pass");
        assert!(!enc(rate, Pass::Second(stats.clone())), "{rate:?} second pass");
    }
    assert!(enc(RateControl::Crf(23.0), Pass::Single));
    assert!(enc(RateControl::Qp(23), Pass::Single));
}

/// The encoder stays serial when GOP-parallel coding is switched off, with one thread, or when
/// two GOPs of `keyint` pictures pass the picture budget.
#[test]
fn serial_when_asked_single_threaded_or_over_the_budget() {
    let enc = |w: u32, h: u32, keyint: u32, threads: usize, on: bool| {
        let mut c = EncoderConfig::new(w, h, 30, 1);
        c.keyint = keyint;
        c.threads = threads;
        c.parallel_gops = on;
        Encoder::new(c).unwrap().gop_parallel()
    };
    assert!(enc(1920, 1080, 60, 4, true), "1080p, 2-second GOPs");
    assert!(!enc(1920, 1080, 60, 4, false), "switched off");
    assert!(!enc(1920, 1080, 60, 1, true), "one thread");
    // two GOPs of 250 1080p pictures (3.6 MiB each) are over the 1 GiB budget
    assert!(!enc(1920, 1080, 250, 4, true), "1080p, keyint 250");
    // 4K pictures are 13 MiB: 20-picture GOPs fit twice, 2-second ones do not
    assert!(enc(3840, 2160, 20, 4, true), "4K, keyint 20");
    assert!(!enc(3840, 2160, 60, 4, true), "4K, keyint 60");
}

/// The first-pass statistics of a CRF encode are the same on the GOP-parallel and serial paths.
#[test]
fn crf_first_pass_statistics_follow_coding_order() {
    let frames = scenes(176, 144, 30);
    let stats = |on: bool| {
        let mut c = EncoderConfig::new(176, 144, 10, 1);
        c.rate = RateControl::Crf(24.0);
        c.pass = Pass::First;
        c.keyint = 6;
        c.threads = 4;
        c.parallel_gops = on;
        let mut enc = Encoder::new(c).unwrap();
        assert_eq!(enc.gop_parallel(), on);
        for (i, f) in frames.iter().enumerate() {
            enc.try_encode(&f.frame(), i as i64).unwrap();
        }
        enc.flush().unwrap();
        enc.pass_stats().unwrap()
    };
    assert_eq!(stats(true), stats(false));
}

/// A refused input picture between good ones loses none of the packets coded so far: the stream is
/// the serial one.
#[test]
fn a_short_frame_keeps_the_packets_of_finished_gops() {
    let frames = scenes(176, 144, 20);
    let mut c = EncoderConfig::new(176, 144, 10, 1);
    c.keyint = 4;
    c.threads = 4;
    let serial = {
        let mut c = c.clone();
        c.parallel_gops = false;
        encode(c, &frames, &[], 0)
    };
    let mut enc = Encoder::new(c).unwrap();
    let mut packets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        packets.extend(enc.try_encode(&f.frame(), 2 * i as i64).unwrap());
        // a picture with planes too small is refused; nothing coded so far is lost
        let bad = YuvFrame { y: &f.y[..10], u: &f.u, v: &f.v, y_stride: f.w, uv_stride: f.w / 2 };
        assert!(matches!(enc.try_encode(&bad, 0), Err(Error::BadFrame)));
    }
    packets.extend(enc.flush().unwrap());
    assert_eq!(packets.len(), serial.packets.len());
    assert!(packets.iter().zip(&serial.packets).all(|(a, b)| a.data == b.data && a.dts == b.dts));
}
