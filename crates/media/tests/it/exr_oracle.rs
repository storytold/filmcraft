//! OpenEXR stills and sequences against ffmpeg (an external test oracle): ffmpeg writes EXR files in
//! every layout its encoder has (RGB, RGBA, luminance; half and float; no, RLE, ZIPS and ZIP
//! compression) and our decode must give exactly the floats ffmpeg's decoder gives.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_frame::PixelData;
use filmcraft_media::sequence::{ImageSequenceSource, Numbered, sequence_frames};
use filmcraft_media::{FrameRequest, MediaKind, MediaSource};
use filmcraft_time::{FrameRate, Tick};

const W: usize = 95; // odd, so nothing lines up by accident
const H: usize = 63;

fn dir() -> PathBuf {
    filmcraft_testkit::fixtures::fixtures_dir("media/exr")
}

/// The filter making a 95×63 picture, translucent when `alpha`.
fn filter(alpha: bool) -> String {
    let a = if alpha { ",format=gbrapf32le,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='(X*2+Y)/250'" } else { "" };
    format!("scale={W}:{H}{a}")
}

/// One EXR still written by ffmpeg (cached across runs).
fn fixture(ff: &Path, pix_fmt: &str, format: &str, compression: &str) -> Option<PathBuf> {
    let out = dir().join(format!("{pix_fmt}_{format}_{compression}.exr"));
    let vf = filter(pix_fmt.starts_with("gbrap"));
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        std::process::Command::new(ff)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=96x64", "-frames:v", "1", "-vf", &vf, "-pix_fmt", pix_fmt])
            .args(["-compression", compression, "-format", format])
            .arg(tmp)
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// A half float's value, computed arithmetically (independent of the decoder's bit manipulation).
fn f16_value(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (e, m) = (i32::from((h >> 10) & 0x1F), f32::from(h & 0x3FF));
    sign * if e == 0 { m * 2f32.powi(-24) } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) }
}

/// ffmpeg's decode of `file` as planar floats in `pix_fmt` (32-bit, or the 16-bit variant ffmpeg
/// decodes half files to natively when `half`: its conversion to 32 bits is not exact), as
/// interleaved RGBA.
fn ffmpeg_rgba(ff: &Path, file: &Path, pix_fmt: &str, half: bool) -> Vec<f32> {
    let fmt = if half { pix_fmt.replace("f32", "f16") } else { pix_fmt.to_string() };
    let out = std::process::Command::new(ff).args(["-v", "error", "-i"]).arg(file).args(["-f", "rawvideo", "-pix_fmt", &fmt, "-"]).output().unwrap();
    assert!(out.status.success(), "ffmpeg decode of {}", file.display());
    let v: Vec<f32> = if half {
        out.stdout.as_chunks::<2>().0.iter().map(|b| f16_value(u16::from_le_bytes(*b))).collect()
    } else {
        out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    };
    let n = W * H;
    let plane = |k: usize| &v[k * n..(k + 1) * n];
    (0..n)
        .flat_map(|i| match pix_fmt {
            "grayf32le" => [plane(0)[i], plane(0)[i], plane(0)[i], 1.0],
            "gbrpf32le" => [plane(2)[i], plane(0)[i], plane(1)[i], 1.0],
            _ => [plane(2)[i], plane(0)[i], plane(1)[i], plane(3)[i]],
        })
        .collect()
}

fn floats(f: &filmcraft_frame::VideoFrame) -> Vec<f32> {
    match &f.data {
        PixelData::RgbaF32(p) => p.to_vec(),
        _ => panic!("EXR frames are float"),
    }
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max)
}

#[test]
fn exr_stills_match_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for pix_fmt in ["gbrpf32le", "gbrapf32le", "grayf32le"] {
        for format in ["float", "half"] {
            for compression in ["none", "rle", "zip1", "zip16"] {
                let name = format!("{pix_fmt} {format} {compression}");
                let Some(f) = fixture(&ff, pix_fmt, format, compression) else {
                    eprintln!("SKIPPED: ffmpeg cannot write {name}");
                    continue;
                };
                let src = filmcraft_media::open_bytes("still.exr", std::fs::read(&f).unwrap().into(), &[]).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(src.info().kind, MediaKind::Still, "{name}");
                let v = src.info().video.clone().unwrap();
                assert_eq!((v.width as usize, v.height as usize), (W, H), "{name}");
                assert_eq!(v.has_alpha, pix_fmt == "gbrapf32le", "{name}");
                let ours = floats(&src.video_frame(FrameRequest::full(Tick(0))).unwrap());
                let d = max_diff(&ours, &ffmpeg_rgba(&ff, &f, pix_fmt, format == "half"));
                assert_eq!(d, 0.0, "{name}: differs by {d}");
                eprintln!("{name}: max diff {d}");
            }
        }
    }
}

#[test]
fn exr_sequence_plays_linear_float_frames() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let d = dir().join("seq_v1");
    let last = d.join("img_0003.exr");
    if !last.exists() {
        let tmp = filmcraft_testkit::fixtures::temp_path(&d);
        std::fs::create_dir_all(&tmp).unwrap();
        let ok = std::process::Command::new(&ff)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=96x64:rate=25", "-frames:v", "3", "-vf", &filter(true)])
            .args(["-pix_fmt", "gbrapf32le", "-compression", "zip16", "-format", "half", "-f", "image2"])
            .arg(tmp.join("img_%04d.exr"))
            .status()
            .is_ok_and(|s| s.success());
        if !ok || std::fs::rename(&tmp, &d).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        if !last.exists() {
            eprintln!("SKIPPED: ffmpeg cannot write an EXR sequence");
            return;
        }
    }
    let first = d.join("img_0001.exr").to_string_lossy().to_string();
    let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    let frames = sequence_frames(&Numbered::parse(&first).unwrap(), &names);
    let src = ImageSequenceSource::new("img_0001", frames, FrameRate::FPS_25, Arc::new(|p: &str| std::fs::read(p))).unwrap();
    assert_eq!(src.info().kind, MediaKind::ImageSequence);
    assert_eq!(src.frame_count(), 3);
    let v = src.info().video.clone().unwrap();
    assert_eq!((v.codec.as_str(), v.pixel_format.as_str(), v.has_alpha), ("EXR sequence", "RGBA 16-bit float", true));
    assert_eq!(v.color.transfer, filmcraft_color::Transfer::Linear);
    for i in [2, 0, 1] {
        let ours = floats(&src.video_frame(FrameRequest::full(FrameRate::FPS_25.tick_of(i))).unwrap());
        let want = ffmpeg_rgba(&ff, &d.join(format!("img_{:04}.exr", i + 1)), "gbrapf32le", true);
        assert!(max_diff(&ours, &want) == 0.0, "frame {i}");
    }
}

/// PXR24 (lossy 24-bit floats; ffmpeg's encoder cannot write it): OpenImageIO writes the files and
/// re-writes them uncompressed (its OpenEXR reader undoes the compression), and our decode of the
/// PXR24 file must give exactly the floats of that uncompressed copy.
#[test]
fn exr_pxr24_matches_openexr() {
    let tool = |args: &[&str]| std::process::Command::new("oiiotool").args(args).status().is_ok_and(|s| s.success());
    if !tool(&["--version"]) {
        eprintln!("SKIPPED: oiiotool is not installed");
        return;
    }
    for (name, depth, channels, size) in [("f4", "float", 4, "95x63"), ("h4", "half", 4, "40x35"), ("f3", "float", 3, "17x16"), ("h3", "half", 3, "16x17")] {
        let packed = dir().join(format!("pxr24_{name}.exr"));
        let plain = dir().join(format!("pxr24_{name}_plain.exr"));
        let ok = |p: &Path| {
            filmcraft_testkit::fixtures::generate(p, |tmp| {
                let t = tmp.to_string_lossy().to_string();
                if p == packed.as_path() {
                    tool(&[
                        "--pattern",
                        "fill:topleft=1,0,0,0.5:topright=0,1,0,0.5:bottomleft=0,0,1,1:bottomright=1,1,1,1",
                        size,
                        &channels.to_string(),
                        "--noise:type=gaussian:mean=0:stddev=0.3",
                        "-d",
                        depth,
                        "--compression",
                        "pxr24",
                        "-o",
                        &t,
                    ])
                } else {
                    tool(&[packed.to_string_lossy().as_ref(), "-d", depth, "--compression", "none", "-o", &t])
                }
            })
        };
        let (Some(packed), Some(_)) = (ok(&packed), ok(&plain)) else {
            eprintln!("SKIPPED: oiiotool cannot write {name}");
            continue;
        };
        let plain = dir().join(format!("pxr24_{name}_plain.exr"));
        let decode = |p: &Path| {
            floats(&filmcraft_media::open_bytes("still.exr", std::fs::read(p).unwrap().into(), &[]).unwrap().video_frame(FrameRequest::full(Tick(0))).unwrap())
        };
        let (ours, want) = (decode(&packed), decode(&plain));
        assert_eq!(max_diff(&ours, &want), 0.0, "{name}");
    }
}
