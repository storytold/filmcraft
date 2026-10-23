//! Bit-exactness of pictures above 8 bits and in 4:2:2 (High 10, High 4:2:2 and their Intra
//! profiles: Sony XAVC S-I, XAVC S long GOP and the like) against ffmpeg's decoder on libx264
//! streams: intra and inter (P and B) pictures, 8 to 10
//! bits, 4:2:0 and 4:2:2, CAVLC and CABAC, every intra macroblock type, the 8x8 transform,
//! scaling matrices, negative QPs (QP'Y below QpBdOffsetY), several slices, odd (cropped) sizes
//! and the deblocking filter on and off. Skipped when ffmpeg is absent.

mod common;

use filmcraft_h264::{Decoder, Picture};
use std::path::PathBuf;

struct Case {
    name: &'static str,
    source: &'static str,
    width: u32,
    height: u32,
    frames: u32,
    /// Extra video filter after the source (noise).
    filter: &'static str,
    pix_fmt: &'static str,
    profile: &'static str,
    x264: &'static str,
}

const NOISE: &str = "noise=alls=12:allf=t+u";

const fn case(name: &'static str, pix_fmt: &'static str, profile: &'static str, x264: &'static str) -> Case {
    Case { name, source: "testsrc2", width: 352, height: 288, frames: 3, filter: NOISE, pix_fmt, profile, x264 }
}

const CASES: &[Case] = &[
    case("hbd_422_10_cabac", "yuv422p10le", "high422", "keyint=1:cabac=1"),
    case("hbd_422_10_cavlc", "yuv422p10le", "high422", "keyint=1:cabac=0"),
    case("hbd_422_10_cavlc_no_deblock", "yuv422p10le", "high422", "keyint=1:cabac=0:no-deblock=1"),
    case("hbd_422_10_cabac_no_deblock", "yuv422p10le", "high422", "keyint=1:cabac=1:no-deblock=1"),
    case("hbd_422_10_deblock_offsets", "yuv422p10le", "high422", "keyint=1:deblock=-2,3"),
    case("hbd_422_10_no8x8dct", "yuv422p10le", "high422", "keyint=1:8x8dct=0"),
    case("hbd_422_10_cqm_cavlc", "yuv422p10le", "high422", "keyint=1:cabac=0:cqm=jvt"),
    case("hbd_422_10_cqm_cabac", "yuv422p10le", "high422", "keyint=1:cabac=1:cqm=jvt"),
    case("hbd_422_10_slices4", "yuv422p10le", "high422", "keyint=1:slices=4"),
    case("hbd_422_10_aq", "yuv422p10le", "high422", "keyint=1:aq-mode=2"),
    // x264's --qp is QP'Y (QPY + 12 at 10 bits): 1..=11 give negative QPY
    Case { source: "mandelbrot", width: 176, height: 144, ..case("hbd_422_10_qp5_negative", "yuv422p10le", "high422", "keyint=1:qp=5") },
    Case { source: "mandelbrot", width: 176, height: 144, ..case("hbd_422_10_qp1_negative_cavlc", "yuv422p10le", "high422", "keyint=1:qp=1:cabac=0") },
    Case { width: 176, height: 144, ..case("hbd_422_10_qp12", "yuv422p10le", "high422", "keyint=1:qp=12") },
    Case { width: 176, height: 144, filter: "", ..case("hbd_422_10_qp51", "yuv422p10le", "high422", "keyint=1:qp=51") },
    Case { width: 176, height: 144, filter: "", ..case("hbd_422_10_qp81", "yuv422p10le", "high422", "keyint=1:qp=81") },
    Case { width: 350, height: 286, ..case("hbd_422_10_cropped", "yuv422p10le", "high422", "keyint=1") },
    Case { width: 1280, height: 720, frames: 2, ..case("hbd_422_10_720p", "yuv422p10le", "high422", "keyint=1:cabac=0") },
    case("hbd_420_10_cabac", "yuv420p10le", "high10", "keyint=1:cabac=1"),
    case("hbd_420_10_cavlc", "yuv420p10le", "high10", "keyint=1:cabac=0"),
    Case { source: "mandelbrot", width: 176, height: 144, ..case("hbd_420_10_qp3_negative", "yuv420p10le", "high10", "keyint=1:qp=3") },
    // inter pictures (P and B): the same formats with reference pictures, B pyramids, direct modes,
    // explicit and implicit weighted prediction, the 8x8 transform, several slices
    Case { width: 176, height: 144, frames: 20, ..case("hbd_p422_10_cavlc", "yuv422p10le", "high422", "bframes=0:cabac=0:keyint=20") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_p422_10_cabac", "yuv422p10le", "high422", "bframes=0:cabac=1:keyint=20") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b422_10_cavlc", "yuv422p10le", "high422", "bframes=3:cabac=0:keyint=20") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b422_10_cabac", "yuv422p10le", "high422", "bframes=3:cabac=1:keyint=20") },
    Case { width: 176, height: 144, frames: 30, ..case("hbd_b422_10_pyramid", "yuv422p10le", "high422", "bframes=4:b-pyramid=normal:ref=4:keyint=30") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b422_10_temporal", "yuv422p10le", "high422", "bframes=3:direct=temporal:weightb=1") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b422_10_spatial", "yuv422p10le", "high422", "bframes=3:direct=spatial") },
    Case {
        width: 176,
        height: 144,
        frames: 20,
        filter: "fade=in:0:20,noise=alls=12:allf=t+u",
        ..case("hbd_p422_10_weightp", "yuv422p10le", "high422", "bframes=0:weightp=2:ref=2")
    },
    Case {
        width: 176,
        height: 144,
        frames: 20,
        filter: "fade=in:0:20,noise=alls=12:allf=t+u",
        ..case("hbd_b422_10_weightb", "yuv422p10le", "high422", "bframes=3:weightb=1")
    },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_p422_10_no_deblock", "yuv422p10le", "high422", "bframes=0:no-deblock=1") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b422_10_no8x8dct", "yuv422p10le", "high422", "bframes=2:8x8dct=0") },
    Case { frames: 12, ..case("hbd_b422_10_slices4", "yuv422p10le", "high422", "bframes=2:slices=4") },
    Case { width: 176, height: 144, frames: 12, ..case("hbd_b422_10_cqm", "yuv422p10le", "high422", "bframes=2:cqm=jvt") },
    Case { source: "mandelbrot", width: 176, height: 144, frames: 10, ..case("hbd_b422_10_qp5_negative", "yuv422p10le", "high422", "bframes=2:qp=5") },
    Case { width: 350, height: 286, frames: 12, ..case("hbd_b422_10_cropped", "yuv422p10le", "high422", "bframes=2") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b420_10_cabac", "yuv420p10le", "high10", "bframes=3:cabac=1") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b420_10_cavlc_weightb", "yuv420p10le", "high10", "bframes=3:cabac=0:weightb=1") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b422_8_cabac", "yuv422p", "high422", "bframes=3:cabac=1") },
    Case { width: 176, height: 144, frames: 20, ..case("hbd_b422_8_cavlc_weighted", "yuv422p", "high422", "bframes=3:cabac=0:weightb=1:weightp=2") },
    Case { width: 1280, height: 720, frames: 6, ..case("hbd_b422_10_720p", "yuv422p10le", "high422", "bframes=2") },
    case("hbd_422_8_cabac", "yuv422p", "high422", "keyint=1:cabac=1"),
    case("hbd_422_8_cavlc", "yuv422p", "high422", "keyint=1:cabac=0"),
    Case { width: 350, height: 286, ..case("hbd_422_8_cropped", "yuv422p", "high422", "keyint=1") },
];

/// Generate (if needed) the stream and ffmpeg's decode of it; `None` when ffmpeg is unavailable.
fn ensure(c: &Case) -> Option<(PathBuf, PathBuf)> {
    let mut vf = format!("{}=size={}x{}:rate=25", c.source, c.width, c.height);
    if !c.filter.is_empty() {
        vf.push(',');
        vf.push_str(c.filter);
    }
    vf.push_str(&format!(",format={}", c.pix_fmt));
    let (h264, yuv) = common::ensure_stream(c.name, &vf, c.frames, &["-profile:v", c.profile, "-x264-params", c.x264], Some(c.pix_fmt))?;
    Some((h264, yuv?))
}

/// The samples of `p` as ffmpeg's rawvideo layout (little-endian 16-bit above 8 bits).
fn raw(p: &Picture) -> Vec<u8> {
    if p.bit_depth > 8 {
        [&p.y_wide, &p.u_wide, &p.v_wide].iter().flat_map(|pl| pl.iter().flat_map(|v| v.to_le_bytes())).collect()
    } else {
        [&p.y, &p.u, &p.v].iter().flat_map(|pl| pl.iter().copied()).collect()
    }
}

fn check(c: &Case) {
    let Some((h264, yuv)) = ensure(c) else { return };
    let reference = std::fs::read(yuv).unwrap();
    // single-threaded, an odd thread count and every core: all bit-exact
    for threads in [1, 3, 0] {
        let pics = match common::decode_file_threads(&h264, threads) {
            Ok(p) => p,
            Err((au, e, partial)) => panic!("{} (threads {threads}): decode error at access unit {au}: {e} (after {} pictures)", c.name, partial.len()),
        };
        let got: Vec<u8> = pics.iter().flat_map(raw).collect();
        assert_eq!(got.len(), reference.len(), "{} (threads {threads}): output size", c.name);
        if got != reference {
            let frame = reference.len() / pics.len().max(1);
            let at = got.iter().zip(&reference).position(|(a, b)| a != b).unwrap_or(0);
            panic!("{} (threads {threads}): differs from ffmpeg, first at byte {at} (frame {}, offset {})", c.name, at / frame, at % frame);
        }
        let (wide, c422) = (pics[0].bit_depth > 8, pics[0].chroma_422);
        assert_eq!((wide, c422), (c.pix_fmt.contains("10"), c.pix_fmt.contains("422")), "{}: picture format", c.name);
        common::check_pts(&pics).unwrap_or_else(|e| panic!("{} (threads {threads}): {e}", c.name));
    }
}

macro_rules! cases {
    ($($test:ident => $name:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                check(CASES.iter().find(|c| c.name == $name).expect("case"));
            }
        )*
    };
}

cases!(
    hbd_422_10_cabac => "hbd_422_10_cabac",
    hbd_422_10_cavlc => "hbd_422_10_cavlc",
    hbd_422_10_cavlc_no_deblock => "hbd_422_10_cavlc_no_deblock",
    hbd_422_10_cabac_no_deblock => "hbd_422_10_cabac_no_deblock",
    hbd_422_10_deblock_offsets => "hbd_422_10_deblock_offsets",
    hbd_422_10_no8x8dct => "hbd_422_10_no8x8dct",
    hbd_422_10_cqm_cavlc => "hbd_422_10_cqm_cavlc",
    hbd_422_10_cqm_cabac => "hbd_422_10_cqm_cabac",
    hbd_422_10_slices4 => "hbd_422_10_slices4",
    hbd_422_10_aq => "hbd_422_10_aq",
    hbd_422_10_qp5_negative => "hbd_422_10_qp5_negative",
    hbd_422_10_qp1_negative_cavlc => "hbd_422_10_qp1_negative_cavlc",
    hbd_422_10_qp12 => "hbd_422_10_qp12",
    hbd_422_10_qp51 => "hbd_422_10_qp51",
    hbd_422_10_qp81 => "hbd_422_10_qp81",
    hbd_422_10_cropped => "hbd_422_10_cropped",
    hbd_422_10_720p => "hbd_422_10_720p",
    hbd_420_10_cabac => "hbd_420_10_cabac",
    hbd_420_10_cavlc => "hbd_420_10_cavlc",
    hbd_420_10_qp3_negative => "hbd_420_10_qp3_negative",
    hbd_p422_10_cavlc => "hbd_p422_10_cavlc",
    hbd_p422_10_cabac => "hbd_p422_10_cabac",
    hbd_b422_10_cavlc => "hbd_b422_10_cavlc",
    hbd_b422_10_cabac => "hbd_b422_10_cabac",
    hbd_b422_10_pyramid => "hbd_b422_10_pyramid",
    hbd_b422_10_temporal => "hbd_b422_10_temporal",
    hbd_b422_10_spatial => "hbd_b422_10_spatial",
    hbd_p422_10_weightp => "hbd_p422_10_weightp",
    hbd_b422_10_weightb => "hbd_b422_10_weightb",
    hbd_p422_10_no_deblock => "hbd_p422_10_no_deblock",
    hbd_b422_10_no8x8dct => "hbd_b422_10_no8x8dct",
    hbd_b422_10_slices4 => "hbd_b422_10_slices4",
    hbd_b422_10_cqm => "hbd_b422_10_cqm",
    hbd_b422_10_qp5_negative => "hbd_b422_10_qp5_negative",
    hbd_b422_10_cropped => "hbd_b422_10_cropped",
    hbd_b420_10_cabac => "hbd_b420_10_cabac",
    hbd_b420_10_cavlc_weightb => "hbd_b420_10_cavlc_weightb",
    hbd_b422_8_cabac => "hbd_b422_8_cabac",
    hbd_b422_8_cavlc_weighted => "hbd_b422_8_cavlc_weighted",
    hbd_b422_10_720p => "hbd_b422_10_720p",
    hbd_422_8_cabac => "hbd_422_8_cabac",
    hbd_422_8_cavlc => "hbd_422_8_cavlc",
    hbd_422_8_cropped => "hbd_422_8_cropped",
);

/// 4:4:4 (High 4:4:4 Predictive) is an `Unsupported` error, not wrong samples.
#[test]
fn high_444_is_unsupported() {
    let Some((h264, _)) =
        common::ensure_stream("hbd_444", "testsrc2=size=64x64:rate=25,format=yuv444p10le", 2, &["-profile:v", "high444", "-x264-params", "keyint=1"], None)
    else {
        return;
    };
    let data = std::fs::read(&h264).unwrap();
    let mut dec = Decoder::with_threads(1);
    let err = dec.decode(&data, 0).err().or_else(|| dec.take_error());
    assert!(matches!(err, Some(filmcraft_h264::Error::Unsupported(_))), "{err:?}");
}

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Damaged 10-bit and 4:2:2 streams (bit flips anywhere after the first bytes, truncation) must
/// never panic or hang, single- and frame-threaded; errors are fine.
#[test]
fn corrupted_high_bit_depth_streams_do_not_panic() {
    for name in [
        "hbd_422_10_cabac",
        "hbd_422_10_cavlc",
        "hbd_420_10_cabac",
        "hbd_422_10_slices4",
        "hbd_422_8_cavlc",
        "hbd_b422_10_cabac",
        "hbd_b422_10_cavlc",
        "hbd_b422_10_weightb",
        "hbd_b420_10_cavlc_weightb",
    ] {
        let c = CASES.iter().find(|c| c.name == name).expect("case");
        let Some((h264, _)) = ensure(c) else { return };
        let data = std::fs::read(h264).unwrap();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ name.len() as u64;
        for round in 0..16 {
            let mut d = data.clone();
            for _ in 0..(1 + round * 4) {
                let pos = 16 + (xorshift(&mut seed) as usize) % (d.len() - 16);
                d[pos] ^= 1 << (xorshift(&mut seed) % 8);
            }
            if round % 4 == 3 {
                d.truncate(d.len() * 2 / 3);
            }
            for threads in [1, 4] {
                let mut dec = Decoder::with_threads(threads);
                for au in common::split_access_units(&d) {
                    let _ = dec.decode(au, 0);
                }
                let _ = dec.flush();
                let _ = dec.take_error();
            }
        }
    }
}
