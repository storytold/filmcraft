//! Radiance HDR stills against ffmpeg (an external test oracle): we write RGBE files in the flat and
//! new run-length forms, and our decode must give the floats ffmpeg's decoder gives.

use std::path::Path;

use filmcraft_frame::PixelData;
use filmcraft_media::FrameRequest;

const W: usize = 70; // 8..=32767, so scan lines may use the new run-length form
const H: usize = 9;

/// A deterministic picture with long runs (so RLE has runs) and noisy stretches (literals).
fn picture() -> Vec<[u8; 4]> {
    let mut s = 0x1234_5678u32;
    (0..W * H)
        .map(|i| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let (x, y) = (i % W, i / W);
            if x < 30 {
                [200, 100, 50, 120 + y as u8]
            } else if i % 3 == 0 {
                [(s >> 24) as u8, 0, 255, 0] // exponent 0: black
            } else {
                [(s >> 24) as u8, (s >> 16) as u8, (s >> 8) as u8, 100 + (s & 31) as u8]
            }
        })
        .collect()
}

fn rle_line(line: &[[u8; 4]]) -> Vec<u8> {
    let n = line.len();
    let mut out = vec![2, 2, (n >> 8) as u8, n as u8];
    for c in 0..4 {
        let ch: Vec<u8> = line.iter().map(|p| p[c]).collect();
        let mut i = 0;
        while i < n {
            let run = ch[i..].iter().take(127).take_while(|&&b| b == ch[i]).count();
            if run >= 3 {
                out.extend([128 + run as u8, ch[i]]);
                i += run;
            } else {
                let mut j = i + 1;
                while j < n && j - i < 128 && !(j + 2 < n && ch[j] == ch[j + 1] && ch[j] == ch[j + 2]) {
                    j += 1;
                }
                out.push((j - i) as u8);
                out.extend_from_slice(&ch[i..j]);
                i = j;
            }
        }
    }
    out
}

fn write(path: &Path, res: &str, body: &[u8]) {
    let mut v = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n{res}\n").into_bytes();
    v.extend_from_slice(body);
    std::fs::write(path, v).unwrap();
}

fn check(ff: &Path, name: &str, res: &str, body: &[u8]) {
    let dir = filmcraft_testkit::fixtures::fixtures_dir("media/hdr");
    let path = dir.join(name);
    write(&path, res, body);
    let out = std::process::Command::new(ff).args(["-v", "error", "-i"]).arg(&path).args(["-f", "rawvideo", "-pix_fmt", "gbrpf32le", "-"]).output().unwrap();
    assert!(out.status.success(), "ffmpeg decode of {name}: {}", String::from_utf8_lossy(&out.stderr));
    let v: Vec<f32> = out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let n = W * H;
    assert_eq!(v.len(), 3 * n, "{name}: ffmpeg frame size");

    let src = filmcraft_media::open_bytes(name, std::fs::read(&path).unwrap().into(), &[]).unwrap();
    let f = src.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
    assert_eq!((f.width as usize, f.height as usize), (W, H));
    let PixelData::RgbaF32(p) = &f.data else { panic!("HDR frames are float") };
    let mut worst = 0f32;
    for i in 0..n {
        // gbrp planes: 0 = G, 1 = B, 2 = R
        for (c, plane) in [2, 0, 1].into_iter().enumerate() {
            let (a, b) = (p[i * 4 + c], v[plane * n + i]);
            worst = worst.max((a - b).abs() / b.abs().max(1e-6));
        }
        assert_eq!(p[i * 4 + 3], 1.0);
    }
    // ffmpeg's decode and ours differ only by float rounding of the same formula
    assert!(worst == 0.0, "{name}: worst relative difference {worst}");
}

#[test]
fn matches_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let pic = picture();
    let rows: Vec<&[[u8; 4]]> = pic.chunks(W).collect();
    let flat: Vec<u8> = pic.iter().flatten().copied().collect();
    check(&ff, "flat.hdr", &format!("-Y {H} +X {W}"), &flat);
    let rle: Vec<u8> = rows.iter().flat_map(|r| rle_line(r)).collect();
    check(&ff, "rle.hdr", &format!("-Y {H} +X {W}"), &rle);
    // Other axis orders are covered by unit tests: ffmpeg reads every file as top-down, which the
    // format description does not (+Y starts at the bottom row).
}
