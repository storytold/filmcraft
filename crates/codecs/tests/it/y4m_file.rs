//! YUV4MPEG2 (`.y4m`) files (#1292): File ▸ Import and the Media Browser offer them, so they must
//! open. Files are built here: a stream header of tagged parameters, then `FRAME` lines each
//! followed by the planes (Y, Cb, Cr, 16-bit little-endian above 8 bits).

use std::sync::Arc;

use filmcraft_color::Range;
use filmcraft_frame::{Chroma, PixelData};
use filmcraft_media::{FrameRequest, MediaKind, SharedSource};
use filmcraft_time::{FrameRate, Tick};

/// A Y4M file: `header` after the signature, `frames` as raw plane bytes, each after `frame_line`.
fn y4m(header: &str, frame_line: &str, frames: &[Vec<u8>]) -> Vec<u8> {
    let mut out = format!("YUV4MPEG2 {header}\n").into_bytes();
    for f in frames {
        out.extend_from_slice(frame_line.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(f);
    }
    out
}

/// One 8-bit frame: every Y sample `y`, every Cb `cb`, every Cr `cr`.
fn planes8(luma: usize, chroma: usize, y: u8, cb: u8, cr: u8) -> Vec<u8> {
    [vec![y; luma], vec![cb; chroma], vec![cr; chroma]].concat()
}

fn open(name: &str, bytes: Vec<u8>) -> SharedSource {
    filmcraft_codecs::open_bytes(name, Arc::from(bytes)).unwrap()
}

/// The middle of frame `k` at `fps`.
fn at(k: i64, fps: i64) -> FrameRequest {
    FrameRequest::full(Tick::from_rational(2 * k + 1, 1, 2 * fps))
}

#[test]
fn y4m_420_opens_as_a_movie_frame_by_frame() {
    // 6 × 4, 4:2:0 (chroma 3 × 2), three frames at 25 fps.
    let frames: Vec<Vec<u8>> = (0..3).map(|k| planes8(24, 6, 16 + 50 * k, 100 + k, 200 - k)).collect();
    let src = open("clip.y4m", y4m("W6 H4 F25:1 Ip A1:1 C420jpeg", "FRAME", &frames));
    let info = src.info();
    assert_eq!(info.kind, MediaKind::Movie);
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.frame_rate), (6, 4, FrameRate::new(25, 1)));
    assert_eq!(info.duration, Tick::from_units(3, 25));
    for k in 0..3u8 {
        let f = src.video_frame(at(i64::from(k), 25)).unwrap();
        let PixelData::Yuv8 { planes, chroma, .. } = &f.data else { panic!("{:?}", f.data) };
        assert_eq!(*chroma, Chroma::C420);
        assert_eq!((planes[0].len(), planes[1].len(), planes[2].len()), (24, 6, 6));
        assert!(planes[0].iter().all(|&s| s == 16 + 50 * k) && planes[1].iter().all(|&s| s == 100 + k) && planes[2].iter().all(|&s| s == 200 - k));
    }
}

#[test]
fn y4m_high_bit_depth_frame_parameters_aspect_and_range() {
    // 4 × 2, 4:2:2 10-bit (little-endian), 30000:1001, 4:3 pixels, full range, and frame lines
    // carrying their own parameters.
    let mut frame = Vec::new();
    for (n, v) in [(8, 940u16), (4, 512), (4, 64)] {
        for _ in 0..n {
            frame.extend(v.to_le_bytes());
        }
    }
    let src = open("ten.y4m", y4m("W4 H2 F30000:1001 It A4:3 C422p10 XCOLORRANGE=FULL", "FRAME Ip XFOO=1", &[frame.clone(), frame]));
    let v = src.info().video.clone().unwrap();
    assert_eq!((v.frame_rate, v.par, v.color.range), (FrameRate::new(30000, 1001), (4, 3), Range::Full));
    let f = src.video_frame(at(1, 30)).unwrap();
    let PixelData::Yuv16 { planes, chroma, bits, .. } = &f.data else { panic!("{:?}", f.data) };
    assert_eq!((*chroma, *bits), (Chroma::C422, 10));
    assert!(planes[0].iter().all(|&s| s == 940) && planes[1].iter().all(|&s| s == 512) && planes[2].iter().all(|&s| s == 64));
}

#[test]
fn y4m_mono_has_neutral_chroma() {
    let src = open("grey.y4m", y4m("W2 H2 F24:1 Cmono", "FRAME", &[vec![77; 4]]));
    let f = src.video_frame(at(0, 24)).unwrap();
    let PixelData::Yuv8 { planes, .. } = &f.data else { panic!("{:?}", f.data) };
    assert!(planes[0].iter().all(|&s| s == 77) && planes[1].iter().chain(planes[2].iter()).all(|&s| s == 128));
}

#[test]
fn large_y4m_files_open_from_a_reader() {
    let bytes: Arc<[u8]> = Arc::from(y4m("W2 H2 F25:1 C444", "FRAME", &[planes8(4, 4, 50, 60, 70), planes8(4, 4, 90, 60, 70)]));
    let reader: filmcraft_media::SharedReader = Arc::new(filmcraft_media::reader::MemReader(bytes));
    let src = filmcraft_media::reader::open_reader("big.y4m", reader, &filmcraft_codecs::reader_openers(), &filmcraft_codecs::openers()).unwrap();
    let f = src.video_frame(at(1, 25)).unwrap();
    let PixelData::Yuv8 { planes, chroma, .. } = &f.data else { panic!("{:?}", f.data) };
    assert_eq!((*chroma, planes[0][0]), (Chroma::C444, 90));
}

#[test]
fn damaged_and_hostile_y4m_files_fail_without_panicking() {
    let good = y4m("W6 H4 F25:1 C420jpeg", "FRAME", &[planes8(24, 6, 1, 2, 3), planes8(24, 6, 4, 5, 6)]);
    let mut cases: Vec<Vec<u8>> = (0..good.len()).map(|n| good[..n].to_vec()).collect();
    for i in 0..good.len() {
        let mut b = good.clone();
        b[i] ^= 0x41;
        cases.push(b);
    }
    for header in [
        "W99999999 H99999999 F25:1",
        "W0 H4 F25:1",
        "W6 H4 F0:0",
        "W6 H4 F25:0",
        "W6 H4 F25:1 C411",
        "W6 H4 F25:1 Cbogus",
        "W6 H4 A0:0 F-5:1",
        "H4 F25:1",
        "W6 H4 F1:9223372036854775807",
    ] {
        cases.push(y4m(header, "FRAME", &[vec![0; 36]]));
    }
    for (i, b) in cases.into_iter().enumerate() {
        let r = std::panic::catch_unwind(|| {
            filmcraft_codecs::open_bytes("damaged.y4m", Arc::from(b)).map(|s| (0..3).filter(|&k| s.video_frame(at(k, 25)).is_ok()).count())
        });
        assert!(r.is_ok(), "case {i} panicked");
    }
}

/// ffmpeg (an external oracle) writes `.y4m` files; every frame we read must equal, byte for byte,
/// the planes ffmpeg's own rawvideo output gives for the same file.
#[test]
fn ffmpeg_y4m_files_match_ffmpeg_rawvideo() {
    let Some(ff) = filmcraft_testkit::oracle::ffmpeg_or_skip("Y4M oracle") else { return };
    let dir = filmcraft_testkit::fixtures::fixtures_dir("codecs/y4m");
    for (pix_fmt, bytes_per_sample) in [("yuv420p", 1usize), ("yuv422p10le", 2)] {
        let y4m = dir.join(format!("testsrc2_{pix_fmt}.y4m"));
        let made = filmcraft_testkit::fixtures::generate(&y4m, |tmp| {
            std::process::Command::new(&ff)
                .args([
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=94x62:rate=25",
                    "-frames:v",
                    "5",
                    "-pix_fmt",
                    pix_fmt,
                    "-strict",
                    "-1",
                    "-f",
                    "yuv4mpegpipe",
                ])
                .arg(tmp)
                .status()
                .is_ok_and(|s| s.success())
        });
        let Some(y4m) = made else { continue };
        let raw =
            std::process::Command::new(&ff).args(["-v", "error", "-i"]).arg(&y4m).args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"]).output().unwrap().stdout;
        let src = open("oracle.y4m", std::fs::read(&y4m).unwrap());
        assert_eq!(src.info().duration, Tick::from_units(5, 25), "{pix_fmt}");
        let frame_len = raw.len() / 5;
        for k in 0..5 {
            let f = src.video_frame(at(k as i64, 25)).unwrap();
            let ours: Vec<u8> = match &f.data {
                PixelData::Yuv8 { planes, .. } => planes.iter().flat_map(|p| p.iter().copied()).collect(),
                PixelData::Yuv16 { planes, .. } => planes.iter().flat_map(|p| p.iter().flat_map(|s| s.to_le_bytes())).collect(),
                other => panic!("{other:?}"),
            };
            assert_eq!(ours.len(), frame_len, "{pix_fmt} frame {k}");
            assert!(ours == raw[k * frame_len..(k + 1) * frame_len], "{pix_fmt} frame {k} differs ({bytes_per_sample} bytes/sample)");
        }
    }
}
