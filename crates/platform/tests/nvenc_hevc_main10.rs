//! NVENC H.265 Main 10 (HDR PQ / HLG) encoding (Windows, NVIDIA GPU with 10-bit HEVC encoding): the
//! pictures that go in are the 10-bit codes `rgbf_to_yuv420_10` makes of float PQ / HLG R'G'B', the
//! pictures that come out of our own HEVC decoder are 10-bit and close to them (smooth ramps keep far more
//! than 256 levels, bright code values survive), the VUI and the HDR10 SEI messages are right, and
//! hostile inputs are errors. Skips (with SKIPPED) only on a machine without a 10-bit HEVC encoder.
#![cfg(any(target_os = "windows", all(target_os = "linux", target_pointer_width = "64")))]

use filmcraft_bitstream::unescape_rbsp;
use filmcraft_export::{ColorSignal, rgbf_to_yuv420_10};
use filmcraft_hevc::params::Sps;
use filmcraft_isobmff::SampleEntry;
use filmcraft_platform::nvenc::export::nvenc_signal;
use filmcraft_platform::nvenc::{Codec, Config, Nvenc, Packet, Profile, Signal, hevc_available, hevc_hdr_available, nal_type};

const W: usize = 1280;
const H: usize = 720;
const FRAMES: usize = 48;

/// NVENC sessions are a limited resource on consumer GPUs: one test at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn config(w: u32, h: u32) -> Config {
    Config {
        width: w,
        height: h,
        fps: (24, 1),
        bitrate_kbps: 12000,
        max_bitrate_kbps: 18000,
        cbr: false,
        keyint: 24,
        profile: Profile::HevcMain10,
        level: None,
        sar: None,
        bframes: true,
    }
}

/// A float HDR test picture (encoded R'G'B', 3 per pixel): the top half is a smooth grey ramp from black to
/// peak (all of it, so PQ passes 1000 nits near 75 % of the way), a box moves across it; the bottom half
/// is bars of saturated BT.2020 colours (the primaries and secondaries at 0.75, beyond BT.709) and dark steps.
fn picture(i: usize) -> Vec<f32> {
    let mut v = vec![0f32; W * H * 3];
    let bars: [[f32; 3]; 8] =
        [[0.75, 0.0, 0.0], [0.0, 0.75, 0.0], [0.0, 0.0, 0.75], [0.0, 0.75, 0.75], [0.75, 0.0, 0.75], [0.75, 0.75, 0.0], [1.0, 1.0, 1.0], [0.05, 0.05, 0.05]];
    let (bx, by) = ((i * 11) % (W - 80), (i * 5) % (H / 2 - 80));
    for y in 0..H {
        for x in 0..W {
            let o = (y * W + x) * 3;
            let px = if y < H / 2 {
                if x >= bx && x < bx + 80 && y >= by && y < by + 80 { [0.9, 0.3, 0.1] } else { [x as f32 / (W - 1) as f32; 3] }
            } else {
                bars[x * 8 / W]
            };
            v[o..o + 3].copy_from_slice(&px);
        }
    }
    v
}

/// The converter's 10-bit planes of a picture.
fn planes(rgb: &[f32], signal: &ColorSignal) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let (kr, kb) = signal.kr_kb();
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    rgbf_to_yuv420_10(rgb, W, H, kr, kb, &mut y, &mut u, &mut v).expect("convert");
    (y, u, v)
}

type Sources = Vec<(Vec<u16>, Vec<u16>, Vec<u16>)>;

fn encode_all(cfg: &Config, signal: &ColorSignal, frames: usize) -> Option<(Nvenc, Vec<Packet>, Sources)> {
    if !hevc_hdr_available() {
        eprintln!("SKIPPED: no NVENC HEVC Main 10 encoder");
        return None;
    }
    let mut enc = Nvenc::with_signal(cfg, &nvenc_signal(signal)).unwrap_or_else(|why| panic!("NVENC Main 10 is available but refused {cfg:?}: {why}"));
    let (mut packets, mut sources) = (Vec::new(), Vec::new());
    for i in 0..frames {
        let src = planes(&picture(i), signal);
        packets.extend(enc.encode_10(&src.0, &src.1, &src.2, i as u64).unwrap());
        sources.push(src);
    }
    packets.extend(enc.flush().unwrap());
    Some((enc, packets, sources))
}

fn psnr10(a: &[u16], b: &[u16]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (1023.0f64 * 1023.0 / mse).log10() }
}

/// The (NAL type, bytes after the 2-byte header) of every NAL unit of a length-prefixed sample.
fn sample_nals(sample: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(len) = sample.get(pos..pos + 4) {
        let n = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        pos += 4;
        let nal = sample.get(pos..pos + n).expect("a NAL unit inside the sample");
        out.push((nal_type(Codec::Hevc, nal).expect("a NAL header"), nal[2..].to_vec()));
        pos += n;
    }
    assert_eq!(pos, sample.len(), "samples are made of whole NAL units");
    out
}

/// The SEI messages (type, payload) of a prefix SEI NAL unit's RBSP.
fn sei_messages(nal_body: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let rbsp = unescape_rbsp(nal_body);
    let (mut out, mut i) = (Vec::new(), 0usize);
    // the RBSP ends with the stop bit byte 0x80 (rbsp_trailing_bits)
    while i < rbsp.len() && rbsp[i] != 0x80 {
        let mut t = 0u32;
        while rbsp[i] == 0xff {
            t += 255;
            i += 1;
        }
        t += u32::from(rbsp[i]);
        i += 1;
        let mut n = 0usize;
        while rbsp[i] == 0xff {
            n += 255;
            i += 1;
        }
        n += usize::from(rbsp[i]);
        i += 1;
        out.push((t, rbsp[i..i + n].to_vec()));
        i += n;
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn round_trip(name: &str, signal: ColorSignal, want_sei: bool) {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((enc, packets, sources)) = encode_all(&config(W as u32, H as u32), &signal, FRAMES) else { return };
    assert!(enc.is_ten_bit());
    assert_eq!(packets.len(), FRAMES, "{name}: one packet per picture");
    let keys: Vec<i64> = packets.iter().filter(|p| p.key).map(|p| p.pts).collect();
    assert_eq!(keys, vec![0, 24], "{name}: IDR pictures");

    // where NVENC put the SEI: every IDR sample has prefix SEI NAL units (type 39) before its slices; nothing else does
    let expected = nvenc_signal(&signal).sei;
    for (k, p) in packets.iter().enumerate() {
        let nals = sample_nals(&p.data);
        let types: Vec<u8> = nals.iter().map(|n| n.0).collect();
        assert!(types.iter().all(|t| !(32..=37).contains(t)), "{name}: packet {k} carries parameter sets {types:?}");
        let seis: Vec<(u32, Vec<u8>)> = nals.iter().filter(|n| n.0 == 39).flat_map(|n| sei_messages(&n.1)).collect();
        if p.key {
            if k < 2 {
                eprintln!("{name}: IDR sample {k} NAL types in order {types:?}");
            }
            let got: Vec<(u32, Vec<u8>)> = seis.clone();
            let want: Vec<(u32, Vec<u8>)> = expected.iter().map(|m| (m.payload_type, m.payload.clone())).collect();
            assert_eq!(got, want, "{name}: SEI messages of the IDR sample {k}");
            let slice_at = types.iter().position(|t| matches!(t, 19 | 20)).expect("an IDR slice");
            assert!(types.iter().take(slice_at).filter(|t| **t == 39).count() >= usize::from(want_sei), "{name}: prefix SEI before the slice");
            assert!(types.iter().all(|t| *t != 40), "{name}: no suffix SEI");
        } else {
            assert!(seis.is_empty(), "{name}: packet {k} (not an IDR) carries SEI {seis:?}");
        }
    }
    assert_eq!(expected.is_empty(), !want_sei);

    // the record and the SPS: Main 10, 10-bit 4:2:0, BT.2020 + the transfer, limited range
    let hvcc = enc.hevc_config().expect("an hvcC record").clone();
    let sps_nal = enc.parameter_sets().0.to_vec();
    eprintln!("{name}: VPS {}\n{name}: SPS {}\n{name}: PPS {}", hex(enc.vps()), hex(&sps_nal), hex(enc.parameter_sets().1));
    assert_eq!((hvcc.general_profile_idc, hvcc.chroma_format_idc, hvcc.bit_depth_luma, hvcc.bit_depth_chroma), (2, 1, 10, 10), "{name}");
    eprintln!(
        "{name}: hvcC level {} compat {:#010x} constraints {:#014x}",
        hvcc.general_level_idc, hvcc.general_profile_compatibility_flags, hvcc.general_constraint_indicator_flags
    );
    let rbsp = unescape_rbsp(&sps_nal[2..]);
    let sps = Sps::parse(&rbsp).unwrap();
    let vui = sps.vui.clone().expect("a VUI");
    assert_eq!((vui.colour_primaries, vui.transfer_characteristics, vui.matrix_coefficients, vui.full_range), (9, signal.transfer, 9, false), "{name}");

    // our decoder: 10-bit planes
    let entry = SampleEntry::hevc(hvcc, W as u16, H as u16);
    let mut dec = filmcraft_codecs::software_video_decoder(&entry).unwrap();
    let mut out = Vec::new();
    for p in &packets {
        out.extend(dec.decode(&p.data, p.pts).unwrap());
    }
    out.extend(dec.flush());
    assert_eq!(out.len(), FRAMES, "{name}");
    let (mut worst_y, mut worst_c) = (99.0f64, 99.0f64);
    let (mut max_code, mut max_ramp_code) = (0u16, 0u16);
    let mut distinct_min = usize::MAX;
    for f in &out {
        let filmcraft_frame::PixelData::Yuv16 { planes: p, bits, .. } = &f.frame.data else { panic!("{name}: 16-bit planes, got {}", f.frame.format_label()) };
        assert_eq!(*bits, 10, "{name}: decoded bit depth");
        assert_eq!((f.frame.width as usize, f.frame.height as usize), (W, H));
        assert!(p.iter().all(|pl| pl.iter().all(|c| *c < 1024)), "{name}: codes are 10-bit values, not MSB-aligned");
        let src = &sources[f.pts as usize];
        worst_y = worst_y.min(psnr10(&p[0], &src.0));
        worst_c = worst_c.min(psnr10(&p[1], &src.1)).min(psnr10(&p[2], &src.2));
        max_code = max_code.max(p[0].iter().copied().max().unwrap_or(0));
        // the ramp: one row of the top half, away from the moving box (row 340 is below it)
        let row = &p[0][340 * W..341 * W];
        max_ramp_code = max_ramp_code.max(row.iter().copied().max().unwrap_or(0));
        let mut levels: Vec<u16> = row.to_vec();
        levels.sort_unstable();
        levels.dedup();
        distinct_min = distinct_min.min(levels.len());
    }
    let src_max = sources.iter().map(|s| s.0[340 * W..341 * W].iter().copied().max().unwrap_or(0)).max().unwrap_or(0);
    eprintln!(
        "{name}: worst luma PSNR {worst_y:.1} dB, worst chroma {worst_c:.1} dB (10-bit scale), ramp row: {distinct_min} distinct decoded levels (min over frames), \
         max decoded ramp code {max_ramp_code} (source {src_max}), max luma code {max_code}, {} bytes",
        packets.iter().map(|p| p.data.len()).sum::<usize>()
    );
    assert!(worst_y > 60.0, "{name}: luma PSNR {worst_y:.1} dB");
    assert!(worst_c > 60.0, "{name}: chroma PSNR {worst_c:.1} dB");
    assert!(distinct_min > 700, "{name}: only {distinct_min} distinct levels on a smooth ramp: not 10-bit");
    assert!(max_ramp_code >= src_max.saturating_sub(4) && max_ramp_code > 900, "{name}: bright codes lost: {max_ramp_code} (source {src_max})");
}

#[test]
fn pq_decodes_to_the_10_bit_source() {
    round_trip("PQ", ColorSignal::PQ, true);
}

#[test]
fn hlg_decodes_to_the_10_bit_source_without_static_metadata() {
    round_trip("HLG", ColorSignal::HLG, false);
}

/// Run `f`, failing the test (not the process) if it panics.
fn calm<T>(what: &str, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => panic!("{what}: panicked"),
    }
}

#[test]
fn main_10_and_main_encoders_refuse_each_others_pictures() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_hdr_available() {
        eprintln!("SKIPPED: no NVENC HEVC Main 10 encoder");
        return;
    }
    let (w, h) = (640usize, 360usize);
    let mut ten = Nvenc::with_signal(&config(w as u32, h as u32), &nvenc_signal(&ColorSignal::PQ)).unwrap();
    let mut eight = Nvenc::new(&Config { profile: Profile::HevcMain, ..config(w as u32, h as u32) }).unwrap();
    assert!(ten.is_ten_bit() && !eight.is_ten_bit());
    let (y8, c8) = (vec![16u8; w * h], vec![128u8; w * h / 4]);
    let (y10, c10) = (vec![64u16; w * h], vec![512u16; w * h / 4]);
    assert!(calm("8-bit into Main 10", || ten.encode(&y8, &c8, &c8, 0)).err().unwrap().contains("Main 10"));
    assert!(calm("10-bit into Main", || eight.encode_10(&y10, &c10, &c10, 0)).err().unwrap().contains("8-bit"));
    // wrong-length planes
    assert!(calm("short luma", || ten.encode_10(&y10[..w * h - 1], &c10, &c10, 0)).is_err());
    assert!(calm("short chroma", || ten.encode_10(&y10, &c10[..c10.len() - 1], &c10, 0)).is_err());
    assert!(calm("empty", || ten.encode_10(&[], &[], &[], 0)).is_err());
    // nothing was submitted by the refusals: the encoders still work
    ten.encode_10(&y10, &c10, &c10, 0).unwrap();
    eight.encode(&y8, &c8, &c8, 0).unwrap();
    assert!(!ten.flush().unwrap().is_empty() && !eight.flush().unwrap().is_empty());
    // H.264 takes no colour signal, no 10-bit profile
    let h264 = Config { profile: Profile::Main, ..config(w as u32, h as u32) };
    assert!(Nvenc::with_signal(&h264, &nvenc_signal(&ColorSignal::PQ)).is_err());
    assert!(Nvenc::with_signal(&h264, &Signal::default()).is_ok() || !filmcraft_platform::nvenc::available());
}

#[test]
fn hostile_main_10_configurations_give_errors_not_crashes() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !hevc_hdr_available() {
        eprintln!("SKIPPED: no NVENC HEVC Main 10 encoder");
        return;
    }
    let base = config(640, 360);
    let sig = nvenc_signal(&ColorSignal::PQ);
    for (w, h) in [(0, 0), (641, 360), (640, 361), (16, 16), (u32::MAX, u32::MAX), (100_000, 64)] {
        assert!(calm("size", || Nvenc::with_signal(&config(w, h), &sig)).is_err(), "{w}x{h}");
    }
    let variants: Vec<(&str, Config, Signal)> = vec![
        ("zero bitrate", Config { bitrate_kbps: 0, max_bitrate_kbps: 0, ..base.clone() }, sig.clone()),
        ("huge bitrate", Config { bitrate_kbps: u32::MAX, max_bitrate_kbps: u32::MAX, ..base.clone() }, sig.clone()),
        ("keyint 1", Config { keyint: 1, ..base.clone() }, sig.clone()),
        ("zero keyint", Config { keyint: 0, ..base.clone() }, sig.clone()),
        ("odd signal codes", base.clone(), Signal { primaries: 255, transfer: 255, matrix: 255, sei: vec![] }),
        (
            "a huge SEI payload",
            base.clone(),
            Signal { sei: vec![filmcraft_platform::nvenc::Sei { payload_type: 137, payload: vec![0xAB; 100_000] }], ..sig.clone() },
        ),
        ("an empty SEI payload", base.clone(), Signal { sei: vec![filmcraft_platform::nvenc::Sei { payload_type: 144, payload: vec![] }], ..sig.clone() }),
    ];
    for (name, c, s) in variants {
        match calm(name, || Nvenc::with_signal(&c, &s)) {
            Ok(mut enc) => {
                let (y, u) = (vec![300u16; 640 * 360], vec![512u16; 640 * 360 / 4]);
                for i in 0..3u64 {
                    let _ = calm(name, || enc.encode_10(&y, &u, &u, i));
                }
                let _ = calm(name, || enc.flush());
            }
            Err(why) => eprintln!("{name}: declined ({why})"),
        }
    }
    // dropped mid-stream with SEI buffers in flight, repeatedly
    for round in 0..8 {
        let mut enc = Nvenc::with_signal(&base, &sig).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let (y, u) = (vec![300u16; 640 * 360], vec![512u16; 640 * 360 / 4]);
        for i in 0..5 {
            enc.encode_10(&y, &u, &u, i).unwrap();
        }
    }
}

#[test]
fn the_probe_follows_the_encoder() {
    let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let hdr = hevc_hdr_available();
    assert!(!hdr || hevc_available(), "Main 10 implies HEVC");
    assert_eq!(hevc_hdr_available(), hdr, "the answer is kept");
    let made = Nvenc::with_signal(&config(640, 360), &Signal::default());
    assert_eq!(made.is_ok(), hdr, "{:?}", made.err());
}
