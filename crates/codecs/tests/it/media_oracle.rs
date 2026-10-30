//! Image sequences, stills and Broadcast WAV against ffmpeg: ffmpeg writes numbered stills (PNG,
//! JPEG, TIFF, BMP, WebP when available, Targa, DPX), single Targa / DPX stills in every layout it
//! has, and BWF files; our frames must equal ffmpeg's decode of the same files (lossless formats
//! exactly, JPEG within a few levels, high bit depths within one 8-bit step), and the BWF
//! TimeReference must give the start timecode.

use crate::common;

use std::path::Path;
use std::sync::Arc;

use common::*;
use filmcraft_media::sequence::{FrameLoader, ImageSequenceSource, Numbered, sequence_frames};
use filmcraft_media::{FrameRequest, MediaKind, MediaSource};
use filmcraft_time::FrameRate;

/// Twelve numbered stills written by ffmpeg, shared by every test that uses `ext` (#125).
///
/// ffmpeg writes into a private temporary directory, which is renamed into place only when all
/// twelve frames are there, so a test never reads a sequence another test is still writing (a
/// half-written PNG decodes as missing, and the sequence then holds the previous frame). A test that
/// loses the race keeps the winner's copy. The `.complete` marker tells a finished cache from a
/// directory left by an older, non-atomic generator (hence the `_v2` name).
fn make_sequence(ff: &Path, ext: &str, codec_args: &[&str]) -> Option<std::path::PathBuf> {
    let d = dir().join(format!("seq_{ext}_v2"));
    let complete = |d: &Path| d.join(".complete").exists();
    if complete(&d) {
        return Some(d);
    }
    let tmp = filmcraft_testkit::fixtures::temp_path(&d);
    std::fs::create_dir_all(&tmp).ok()?;
    let pattern = tmp.join(format!("img_%04d.{ext}"));
    let mut args = vec!["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=96x64:rate=25", "-frames:v", "12"];
    args.extend_from_slice(codec_args);
    args.extend_from_slice(&["-f", "image2"]);
    let made = std::process::Command::new(ff).args(&args).arg(&pattern).status().is_ok_and(|st| st.success())
        && (1..=12).all(|n| tmp.join(format!("img_{n:04}.{ext}")).exists())
        && std::fs::write(tmp.join(".complete"), b"").is_ok();
    if made && std::fs::rename(&tmp, &d).is_ok() {
        return Some(d);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    complete(&d).then_some(d)
}

fn loader() -> FrameLoader {
    Arc::new(|p: &str| std::fs::read(p))
}

fn open_sequence(d: &Path, ext: &str) -> ImageSequenceSource {
    let first = d.join(format!("img_0001.{ext}")).to_string_lossy().to_string();
    let n = Numbered::parse(&first).unwrap();
    let names: Vec<String> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    ImageSequenceSource::new("img_0001", sequence_frames(&n, &names), FrameRate::FPS_25, loader()).unwrap()
}

fn rgba(f: &filmcraft_frame::VideoFrame) -> Vec<u8> {
    match &f.data {
        filmcraft_frame::PixelData::Rgba8(p) => p.to_vec(),
        _ => panic!("still frames are RGBA"),
    }
}

fn check(ff: &Path, ext: &str, codec_args: &[&str], tol: u8) {
    let Some(d) = make_sequence(ff, ext, codec_args) else {
        eprintln!("SKIPPED: ffmpeg cannot write {ext} stills");
        return;
    };
    let src = open_sequence(&d, ext);
    assert_eq!(src.info().kind, MediaKind::ImageSequence);
    assert_eq!(src.frame_count(), 12);
    assert_eq!(src.info().duration, FrameRate::FPS_25.tick_of(12));
    let pattern = d.join(format!("img_%04d.{ext}"));
    let raw = ffmpeg_out(ff, &["-f", "image2", "-start_number", "1", "-i", pattern.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
    let fsize = 96 * 64 * 4;
    assert_eq!(raw.len() / fsize, 12);
    let mut rng = Rng(5);
    let mut worst = 0u8;
    // in order, then random access
    let order: Vec<usize> = (0..12).chain((0..12).map(|_| rng.below(12) as usize)).collect();
    for i in order {
        let f = src.video_frame(FrameRequest::full(FrameRate::FPS_25.tick_of(i as i64))).unwrap();
        let ours = rgba(&f);
        let want = &raw[i * fsize..(i + 1) * fsize];
        let d = ours.iter().zip(want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        worst = worst.max(d);
        assert!(d <= tol, "{ext}: frame {i} differs by {d}");
    }
    eprintln!("{ext} sequence: 12 frames, max diff {worst}");
}

#[test]
fn png_sequence_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check(&ff, "png", &[], 0);
}

#[test]
fn tiff_and_bmp_sequences_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check(&ff, "tif", &["-pix_fmt", "rgb24"], 0);
    check(&ff, "bmp", &[], 0);
}

#[test]
fn jpeg_sequence_close() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    // different IDCT and chroma upsampling than ffmpeg's: a few levels
    check(&ff, "jpg", &["-q:v", "2", "-pix_fmt", "yuvj444p"], 12);
}

#[test]
fn webp_sequence_lossless() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check(&ff, "webp", &["-c:v", "libwebp", "-lossless", "1"], 0);
}

#[test]
fn targa_and_dpx_sequences_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check(&ff, "tga", &[], 0);
    check(&ff, "dpx", &["-pix_fmt", "gbrp10be"], 1);
}

/// One `ext` still in `pix_fmt` (95 × 63, so scan lines need padding; translucent when the format
/// has alpha) opened like an imported file must match ffmpeg's RGBA decode within `tol` (exactly
/// when `tol` is 0).
fn still_matches(ff: &Path, ext: &str, pix_fmt: &str, extra: &[&str], tol: u8) {
    let name = format!("still_{pix_fmt}{}.{ext}", extra.join(""));
    let alpha = if pix_fmt.contains('a') && pix_fmt != "pal8" { ",format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='X*2+Y'" } else { "" };
    let vf = format!("scale=95:63{alpha}");
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=96x64", "-frames:v", "1", "-vf", &vf, "-pix_fmt", pix_fmt];
    args.extend_from_slice(extra);
    let Some(f) = fixture(ff, &name, &args) else {
        eprintln!("SKIPPED: ffmpeg cannot write {name}");
        return;
    };
    let src = filmcraft_codecs::open_bytes(&name, bytes(&f)).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(src.info().kind, MediaKind::Still, "{name}");
    let v = src.info().video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (95, 63), "{name}");
    let ours = rgba(&src.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap());
    // deeper than 8 bits: ffmpeg's 16-bit decode, rounded to 8 bits (its own 8-bit conversion dithers)
    let want: Vec<u8> = if tol == 0 {
        ffmpeg_out(ff, &["-i", f.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
    } else {
        let wide = ffmpeg_out(ff, &["-i", f.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgba64le", "-"]);
        wide.as_chunks::<2>().0.iter().map(|c| ((u32::from(u16::from_le_bytes(*c)) * 255 + 32767) / 65535) as u8).collect()
    };
    assert_eq!(ours.len(), want.len(), "{name}");
    let d = ours.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(d <= tol, "{name}: differs by {d}");
    assert_eq!(v.has_alpha, want.as_chunks::<4>().0.iter().any(|p| p[3] != 255), "{name}");
    eprintln!("{name}: max diff {d}");
}

#[test]
fn targa_stills_every_layout() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for pix_fmt in ["bgr24", "bgra", "rgb555le", "gray", "pal8"] {
        still_matches(&ff, "tga", pix_fmt, &[], 0); // run-length encoded (ffmpeg's default)
        still_matches(&ff, "tga", pix_fmt, &["-rle", "0"], 0);
    }
}

#[test]
fn dpx_stills_every_layout() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (pix_fmt, tol) in [("rgb24", 0), ("rgba", 0), ("gray", 0), ("rgb48be", 1), ("rgba64be", 1), ("gray16be", 1), ("gbrp10be", 1), ("gbrp12be", 1)] {
        still_matches(&ff, "dpx", pix_fmt, &[], tol);
    }
}

/// A big-endian DPX file holding `data` as its one image element (`w` × `h`, the given
/// descriptor, bit depth, packing and colorimetric code; reference codes undefined).
fn dpx_file(w: u32, h: u32, descriptor: u8, bits: u8, packing: u16, colorimetric: u8, data: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 2048];
    let put = |f: &mut Vec<u8>, at: usize, v: &[u8]| f[at..at + v.len()].copy_from_slice(v);
    put(&mut f, 0, b"SDPX");
    put(&mut f, 4, &2048u32.to_be_bytes());
    put(&mut f, 8, b"V2.0");
    put(&mut f, 16, &(2048 + data.len() as u32).to_be_bytes());
    put(&mut f, 770, &1u16.to_be_bytes());
    put(&mut f, 772, &w.to_be_bytes());
    put(&mut f, 776, &h.to_be_bytes());
    let el = 780;
    put(&mut f, el + 4, &u32::MAX.to_be_bytes());
    put(&mut f, el + 12, &u32::MAX.to_be_bytes());
    f[el + 20] = descriptor;
    f[el + 22] = colorimetric;
    f[el + 23] = bits;
    put(&mut f, el + 24, &packing.to_be_bytes());
    put(&mut f, el + 28, &2048u32.to_be_bytes());
    f.extend_from_slice(data);
    f
}

/// `pix_fmt` raw frame of ffmpeg's test picture at `w` × `h`.
fn raw_picture(ff: &Path, w: u32, h: u32, pix_fmt: &str) -> Vec<u8> {
    let alpha = if pix_fmt.contains('a') { ",format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='X*2+Y'" } else { "" };
    let vf = format!("scale={w}:{h}{alpha}");
    ffmpeg_out(ff, &["-f", "lavfi", "-i", "testsrc2=size=96x64", "-frames:v", "1", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
}

/// A DPX file we built must import exactly like ffmpeg decodes it, within `tol` of its 16-bit
/// decode rounded to 8 bits.
fn dpx_matches(ff: &Path, name: &str, file: Vec<u8>, tol: u8) {
    let path = dir().join(name);
    std::fs::create_dir_all(dir()).unwrap();
    std::fs::write(&path, &file).unwrap();
    let src = filmcraft_codecs::open_bytes(name, file.into()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(src.info().kind, MediaKind::Still, "{name}");
    let ours = rgba(&src.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap());
    let wide = ffmpeg_out(ff, &["-i", path.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgba64le", "-"]);
    let want: Vec<u8> = wide.as_chunks::<2>().0.iter().map(|c| ((u32::from(u16::from_le_bytes(*c)) * 255 + 32767) / 65535) as u8).collect();
    assert_eq!(ours.len(), want.len(), "{name}");
    let d = ours.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(d <= tol, "{name}: differs by {d}");
    eprintln!("{name}: max diff {d}");
}

/// Packed 12-bit RGB (packing 0: components back to back, low bits first, lines on 32-bit
/// boundaries) and 8-bit Y'CbCr (4:2:2, 4:4:4, 4:4:4:4 with alpha; BT.709, BT.601 and unspecified
/// colorimetry), layouts ffmpeg reads but does not write, built here from ffmpeg's test picture
/// (#841).
#[test]
fn dpx_packed_and_ycbcr_match_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let (w, h) = (95u32, 63u32);
    let rgb: Vec<u32> = raw_picture(&ff, w, h, "rgb48le").as_chunks::<2>().0.iter().map(|c| u32::from(u16::from_le_bytes(*c)) >> 4).collect();
    let mut data = Vec::new();
    for line in rgb.chunks(w as usize * 3) {
        let (mut acc, mut n) = (0u64, 0u32);
        for &v in line {
            acc |= u64::from(v) << n;
            n += 12;
            while n >= 32 {
                data.extend_from_slice(&(acc as u32).to_be_bytes());
                acc >>= 32;
                n -= 32;
            }
        }
        if n > 0 {
            data.extend_from_slice(&(acc as u32).to_be_bytes());
        }
    }
    dpx_matches(&ff, "dpx_packed12.dpx", dpx_file(w, h, 50, 12, 0, 6, &data), 1);

    // Y'CbCr: even width, scan lines a multiple of 4 bytes
    let (w, h) = (96u32, 64u32);
    let px = (w * h) as usize;
    let uyvy = raw_picture(&ff, w, h, "uyvy422");
    let yuv = raw_picture(&ff, w, h, "yuv444p");
    let yuva = raw_picture(&ff, w, h, "yuva444p");
    let interleave = |planes: &[u8], order: &[usize]| -> Vec<u8> { (0..px).flat_map(|i| order.iter().map(move |&p| planes[p * px + i])).collect() };
    // planes are Y, U (Cb), V (Cr), A; DPX stores Cb Y Cr A
    let cbycr = interleave(&yuv, &[1, 0, 2]);
    let cbycra = interleave(&yuva, &[1, 0, 2, 3]);
    for colorimetric in [6u8, 7, 0] {
        dpx_matches(&ff, &format!("dpx_cbycry_{colorimetric}.dpx"), dpx_file(w, h, 100, 8, 0, colorimetric, &uyvy), 1);
        dpx_matches(&ff, &format!("dpx_cbycr_{colorimetric}.dpx"), dpx_file(w, h, 102, 8, 0, colorimetric, &cbycr), 1);
    }
    dpx_matches(&ff, "dpx_cbycra.dpx", dpx_file(w, h, 103, 8, 0, 6, &cbycra), 1);
}

#[test]
fn missing_frames_hold_the_previous_frame() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(d) = make_sequence(&ff, "png", &[]) else { return };
    // a private copy without frames 5 and 6
    let gap = filmcraft_testkit::fixtures::temp_path(&dir().join("seq_png_gap"));
    std::fs::create_dir_all(&gap).unwrap();
    for n in (1..=12).filter(|n| *n != 5 && *n != 6) {
        let name = format!("img_{n:04}.png");
        std::fs::copy(d.join(&name), gap.join(&name)).unwrap();
    }
    let _ = std::fs::remove_file(gap.join("img_0005.png"));
    let _ = std::fs::remove_file(gap.join("img_0006.png"));
    let src = open_sequence(&gap, "png");
    assert_eq!(src.missing_frames(), vec![4, 5]);
    let at = |i: i64| rgba(&src.video_frame(FrameRequest::full(FrameRate::FPS_25.tick_of(i))).unwrap());
    assert_eq!(at(4), at(3));
    assert_eq!(at(5), at(3));
    assert_ne!(at(6), at(3));
    let _ = std::fs::remove_dir_all(&gap);
}

#[test]
fn broadcast_wav_time_reference() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    // 01:00:00:00 at 23.976: 86 400 frames × 2002 samples
    let tr = (86_400u64 * 2002).to_string();
    let meta = format!("time_reference={tr}");
    let Some(f) = fixture(
        &ff,
        "bwf_1h.wav",
        &["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000", "-t", "0.5", "-c:a", "pcm_s24le", "-write_bext", "1", "-metadata", &meta],
    ) else {
        panic!("could not write BWF")
    };
    let src = filmcraft_codecs::open_bytes("bwf_1h.wav", bytes(&f)).unwrap();
    assert_eq!(src.info().container, "Broadcast WAV");
    assert_eq!(src.info().start_timecode, Some(86_400));
    // ffprobe agrees on the time reference
    if let Some(fp) = filmcraft_testkit::ffprobe() {
        let o = std::process::Command::new(fp).args(["-v", "error", "-show_entries", "format_tags=time_reference", "-of", "csv=p=0"]).arg(&f).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), tr);
    }
    // samples are exact
    let want = ffmpeg_audio_f32(&ff, &f, &[]);
    let got = src.audio(0, want.len(), 48_000).unwrap();
    assert_eq!(got.channels[0], want);
}
