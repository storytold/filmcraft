//! Decode ffmpeg-made NTSC DV25 fixtures and compare with ffmpeg's own decode. FFmpeg is an
//! external test oracle and fixture generator only; the decoder has no dependencies or subprocesses.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use filmcraft_dv::decode_frame;

const PACKET: usize = 120000;
const FRAME: usize = 720 * 480 + 2 * 180 * 480;
const FRAMES: usize = 6;

/// Offsets of the six DCT block headers in a video DIF block (four 14-byte Y, then Cr and Cb).
const BLOCKS: [usize; 6] = [4, 18, 32, 46, 60, 70];

struct Spec {
    name: &'static str,
    source: &'static str,
    /// Extra dvvideo encoder flags.
    flags: &'static [&'static str],
    /// Switch every chroma block to the 2-4-8 DCT before decoding (see [`force_248_chroma`]).
    field_chroma: bool,
}

const CHROMA: &str = "nullsrc=size=720x480:rate=30000/1001,geq=lum='128+100*sin(X*0.4)*sin(Y*0.7)':cb='128+60*sin(X*0.2)':cr='128+60*cos(Y*0.3)'";

fn specs() -> Vec<Spec> {
    let s = |name, source, flags, field_chroma| Spec { name, source, flags, field_chroma };
    vec![
        s("testsrc2", "testsrc2=size=720x480:rate=30000/1001", &[], false),
        s("black", "color=black:size=720x480:rate=30000/1001", &[], false),
        s("white", "color=white:size=720x480:rate=30000/1001", &[], false),
        s("smptebars", "smptebars=size=720x480:rate=30000/1001", &[], false),
        s("chroma", CHROMA, &[], false),
        // Without `+ildct` FFmpeg codes every block with the 8x8 DCT, interlaced or not.
        s("interlaced_ildct", "testsrc2=size=720x480:rate=60000/1001,tinterlace=mode=interleave_bottom", &["-flags", "+ildct"], false),
        s("testsrc2_248chroma", "testsrc2=size=720x480:rate=30000/1001", &[], true),
        s("chroma_248chroma", CHROMA, &[], true),
    ]
}

fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("dv oracle")
}

fn run(ff: &Path, args: &[&str], out: &Path) -> bool {
    let st = Command::new(ff).args(["-nostdin", "-y", "-v", "error"]).args(args).arg(out).stdin(Stdio::null()).status();
    st.is_ok_and(|s| s.success())
}

/// Video DIF blocks (section type 4) of a DIF stream.
fn video_dif_blocks(dv: &mut [u8]) -> impl Iterator<Item = &mut [u8; 80]> {
    dv.as_chunks_mut::<80>().0.iter_mut().filter(|d| d[0] >> 5 == 4)
}

/// Number of (luma, chroma) blocks coded with the 2-4-8 DCT (header mode bit set).
fn dct248_blocks(dv: &[u8]) -> (usize, usize) {
    let mut dv = dv.to_vec();
    let mut count = (0, 0);
    for d in video_dif_blocks(&mut dv) {
        for (b, &o) in BLOCKS.iter().enumerate() {
            if d[o + 1] & 0x40 != 0 {
                if b < 4 { count.0 += 1 } else { count.1 += 1 }
            }
        }
    }
    count
}

/// FFmpeg's encoder never codes chroma with the 2-4-8 DCT, but camera footage does. Setting the
/// mode bit keeps the stream valid and makes both decoders read the coefficients as 2-4-8.
fn force_248_chroma(dv: &mut [u8]) {
    for d in video_dif_blocks(dv) {
        for &o in &BLOCKS[4..] {
            d[o + 1] |= 0x40;
        }
    }
}

/// The spec's DIF stream (cached under `target/fixtures/dv/`).
fn make(ff: &Path, spec: &Spec) -> PathBuf {
    let dir = filmcraft_testkit::fixtures_dir("dv");
    let out = dir.join(format!("{}.dv", spec.name));
    let made = filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let frames = FRAMES.to_string();
        let mut args = vec!["-f", "lavfi", "-i", spec.source, "-frames:v", &frames, "-pix_fmt", "yuv411p", "-c:v", "dvvideo"];
        args.extend_from_slice(spec.flags);
        args.extend_from_slice(&["-threads", "1", "-an", "-f", "dv"]);
        if !run(ff, &args, tmp) {
            return false;
        }
        if spec.field_chroma {
            let mut dv = std::fs::read(tmp).unwrap();
            force_248_chroma(&mut dv);
            std::fs::write(tmp, dv).unwrap();
        }
        true
    });
    made.unwrap_or_else(|| panic!("{}: could not generate the fixture", spec.name))
}

/// ffmpeg's decode of `dv` as planar 4:1:1 (cached next to it).
fn reference(ff: &Path, dv: &Path) -> Vec<u8> {
    let out = dv.with_extension("yuv411p.yuv");
    let input = dv.to_str().unwrap();
    let made = filmcraft_testkit::fixtures::generate(&out, |tmp| run(ff, &["-i", input, "-pix_fmt", "yuv411p", "-f", "rawvideo"], tmp));
    std::fs::read(made.unwrap_or_else(|| panic!("{}: ffmpeg could not decode it", dv.display()))).unwrap()
}

/// (maximum sample error, mean squared error) over `a` and `b`, or only their rightmost 4 chroma
/// columns (the clipped 4x16 chroma blocks of the 704..720 macroblocks).
fn error(a: &[u8], b: &[u8], right_chroma: bool) -> (u8, f64) {
    let pairs: Vec<(u8, u8)> = if right_chroma {
        (0..a.len() / FRAME)
            .flat_map(|f| (0..2 * 480).map(move |row| f * FRAME + 720 * 480 + row * 180))
            .flat_map(|row| (176..180).map(move |x| row + x))
            .map(|i| (a[i], b[i]))
            .collect()
    } else {
        a.iter().copied().zip(b.iter().copied()).collect()
    };
    let max = pairs.iter().map(|&(a, b)| a.abs_diff(b)).max().unwrap();
    let mse = pairs.iter().map(|&(a, b)| (f64::from(a) - f64::from(b)).powi(2)).sum::<f64>() / pairs.len() as f64;
    (max, mse)
}

/// Generate every fixture (used by `cargo xtask fixtures dv`).
#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = ffmpeg() else { return };
    for spec in specs() {
        let dv = filmcraft_testkit::fixtures_dir("dv").join(format!("{}.dv", spec.name));
        let outputs = [dv.clone(), dv.with_extension("yuv411p.yuv")];
        filmcraft_testkit::fixtures::generate_and_report(spec.name, &outputs, || Some(reference(&ff, &make(&ff, &spec))));
    }
}

#[test]
fn decode_matches_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    for spec in specs() {
        let dv = make(&ff, &spec);
        let input = std::fs::read(&dv).unwrap();
        let reference = reference(&ff, &dv);
        // Guard the coverage itself: an encoder change must not quietly drop the 2-4-8 cases.
        let (luma_248, chroma_248) = dct248_blocks(&input);
        if !spec.flags.is_empty() {
            assert!(luma_248 > 0, "{}: no 2-4-8 luma blocks", spec.name);
        }
        if spec.field_chroma {
            assert_eq!(chroma_248, 2 * 1350 * FRAMES, "{}", spec.name);
        }
        let mut output = Vec::new();
        for packet in input.as_chunks::<PACKET>().0 {
            let f = decode_frame(packet).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            output.extend(f.y);
            output.extend(f.cb);
            output.extend(f.cr);
        }
        assert_eq!(output.len(), FRAMES * FRAME, "{}", spec.name);
        assert_eq!(output.len(), reference.len(), "{}", spec.name);
        let (max, mse) = error(&output, &reference, false);
        let (edge_max, edge_mse) = error(&output, &reference, true);
        println!("{:<20} max {max} mse {mse:.4} | rightmost chroma max {edge_max} mse {edge_mse:.4} | 2-4-8 blocks: {luma_248} Y, {chroma_248} C", spec.name);
        assert!(max <= 3 && mse < 0.5, "{}: max={max}, mse={mse}", spec.name);
        assert!(edge_max <= 3 && edge_mse < 0.5, "{}, rightmost chroma: max={edge_max}, mse={edge_mse}", spec.name);
    }
}

#[test]
fn mutated_and_truncated_frames_never_panic() {
    let Some(ff) = ffmpeg() else { return };
    for spec in specs() {
        let input = std::fs::read(make(&ff, &spec)).unwrap();
        // Mutate arbitrary DIF IDs, headers, lengths and VLC payload bits of an actual frame.
        let frame = &input[..PACKET];
        let mut state = 0x9387ac34u32;
        for _ in 0..96 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let mut corrupted = frame.to_vec();
            let pos = state as usize % corrupted.len();
            corrupted[pos] ^= 1 << (state % 8);
            assert!(std::panic::catch_unwind(|| decode_frame(&corrupted)).is_ok(), "{}", spec.name);
        }
        for n in [1, 79, 80, PACKET - 1] {
            assert!(decode_frame(&frame[..n]).is_err(), "{}", spec.name);
        }
    }
}
