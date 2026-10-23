//! H.264 High 10 / High 4:2:2 intra video (Sony XAVC S-I and friends) through the whole media
//! stack, against ffmpeg: the pictures come out as 16-bit (or 8-bit 4:2:2) planar frames equal to
//! ffmpeg's, with the colour the stream signals.

mod common;

use common::*;
use filmcraft_color::{Matrix, Primaries, Transfer};
use filmcraft_frame::{Chroma, PixelData};
use filmcraft_media::FrameRequest;

/// Our frames of `name` at every frame index against ffmpeg's decode as `pix_fmt`; returns the
/// last frame for further checks.
fn check_intra(
    ff: &std::path::Path,
    name: &str,
    pix_fmt: &str,
    bps: usize,
    chroma: Chroma,
    bits: Option<u32>,
    x264: &str,
    size: (usize, usize),
) -> std::sync::Arc<filmcraft_frame::VideoFrame> {
    let (w, h) = size;
    let args = [
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size={w}x{h}:rate=25,noise=alls=10:allf=t+u"),
        "-frames:v",
        "6",
        "-c:v",
        "libx264",
        "-pix_fmt",
        pix_fmt,
        "-x264-params",
        x264,
    ];
    let path = fixture(ff, name, &args).expect("fixture");
    let want = ffmpeg_frames(ff, &path, pix_fmt);
    let (cw, ch) = if chroma == Chroma::C422 { (w.div_ceil(2), h) } else { (w.div_ceil(2), h.div_ceil(2)) };
    let frame_bytes = (w * h + 2 * cw * ch) * bps;
    assert_eq!(want.len(), 6 * frame_bytes, "{name}: ffmpeg frame count");
    let src = filmcraft_codecs::open_bytes(name, bytes(&path)).unwrap();
    let rate = src.info().frame_rate();
    let mut last = None;
    for i in 0..6 {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap();
        match &f.data {
            PixelData::Yuv16 { chroma: c, bits: b, .. } => assert_eq!((*c, Some(*b)), (chroma, bits), "{name}: frame format"),
            PixelData::Yuv8 { chroma: c, .. } => assert_eq!((*c, None), (chroma, bits), "{name}: frame format"),
            _ => panic!("{name}: expected planar YUV"),
        }
        let got = planes(&f);
        let exp = raw_planes(&want[i as usize * frame_bytes..(i as usize + 1) * frame_bytes], w, h, cw, ch, bps);
        assert_eq!(max_diff(&got, &exp), 0, "{name}: frame {i} differs from ffmpeg");
        last = Some(f);
    }
    last.unwrap()
}

#[test]
fn high_422_10_bit_intra_equals_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (cabac, name) in [(0, "i422_10_cavlc.mp4"), (1, "i422_10_cabac.mp4")] {
        check_intra(&ff, name, "yuv422p10le", 2, Chroma::C422, Some(10), &format!("keyint=1:cabac={cabac}"), (176, 144));
    }
}

#[test]
fn high_10_420_intra_equals_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check_intra(&ff, "i420_10_cabac.mp4", "yuv420p10le", 2, Chroma::C420, Some(10), "keyint=1", (176, 144));
}

#[test]
fn high_422_8_bit_intra_equals_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check_intra(&ff, "i422_8_cavlc.mp4", "yuv422p", 1, Chroma::C422, None, "keyint=1:cabac=0", (176, 144));
}

/// A frame size that is not a multiple of the macroblock size crops the 4:2:2 chroma planes with
/// the 4:2:2 crop units (2 × 1).
#[test]
fn high_422_10_bit_cropped_intra_equals_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = check_intra(&ff, "i422_10_odd.mp4", "yuv422p10le", 2, Chroma::C422, Some(10), "keyint=1", (178, 142));
    assert_eq!((f.width, f.height), (178, 142));
}

/// What the camera signals (BT.2020, HLG) reaches the frame.
#[test]
fn high_422_10_bit_hlg_colour_is_kept() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=25",
        "-frames:v",
        "2",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv422p10le",
        "-x264-params",
        "keyint=1:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
    ];
    let path = fixture(&ff, "i422_10_hlg.mp4", &args).expect("fixture");
    let src = filmcraft_codecs::open_bytes("i422_10_hlg.mp4", bytes(&path)).unwrap();
    let f = src.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
    assert_eq!((f.color.transfer, f.color.primaries, f.color.matrix), (Transfer::Hlg, Primaries::Bt2020, Matrix::Bt2020Ncl));
    let label = src.info().video.as_ref().unwrap().pixel_format.clone();
    assert!(label.contains("4:2:2") && label.contains("10-bit"), "pixel format label {label:?}");
}

/// Long-GOP High 4:2:2 10-bit and High 10 (P and B pictures, reordered output; XAVC S) through the
/// MP4 reader equals ffmpeg frame for frame.
#[test]
fn long_gop_high_bit_depth_equals_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check_intra(&ff, "p422_10_gop_cabac.mp4", "yuv422p10le", 2, Chroma::C422, Some(10), "keyint=6:bframes=2:cabac=1", (176, 144));
    check_intra(&ff, "p422_10_gop_cavlc.mp4", "yuv422p10le", 2, Chroma::C422, Some(10), "keyint=6:bframes=2:cabac=0:weightb=1", (176, 144));
    check_intra(&ff, "p420_10_gop.mp4", "yuv420p10le", 2, Chroma::C420, Some(10), "keyint=6:bframes=2", (176, 144));
}

/// A real camera clip (Sony A7R V XAVC S-I: 3840x2160 4:2:2 10-bit All-Intra, HLG) opened from
/// disk like the app does and compared with ffmpeg on a few frames:
/// `FILMCRAFT_XAVC_CLIP=path/to/clip.mp4 cargo test -p filmcraft-codecs --test h264_high_bit_depth_oracle -- --ignored`.
#[test]
#[ignore = "needs FILMCRAFT_XAVC_CLIP"]
fn camera_clip_equals_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = std::env::var_os("FILMCRAFT_XAVC_CLIP").map(std::path::PathBuf::from) else {
        eprintln!("SKIPPED: FILMCRAFT_XAVC_CLIP is not set");
        return;
    };
    let reader: filmcraft_media::SharedReader = std::sync::Arc::new(filmcraft_media::reader::FileReader::open(&path).unwrap());
    let src = filmcraft_media::reader::open_reader("clip.mp4", reader, &filmcraft_codecs::reader_openers(), &filmcraft_codecs::openers()).unwrap();
    let info = src.info().video.clone().unwrap();
    eprintln!("{}x{} {} {}", info.width, info.height, info.codec, info.pixel_format);
    let rate = src.info().frame_rate();
    let (w, h) = (info.width as usize, info.height as usize);
    for n in [0i64, 1, 59, 300] {
        let sel = format!(r"select=eq(n\,{n}),format=yuv422p10le");
        let want =
            ffmpeg_out(&ff, &["-i", path.to_str().unwrap(), "-map", "0:v:0", "-vf", &sel, "-frames:v", "1", "-fps_mode", "passthrough", "-f", "rawvideo", "-"]);
        let t = std::time::Instant::now();
        let f = src.video_frame(FrameRequest::full(rate.tick_of(n))).unwrap();
        eprintln!("frame {n}: {:?}", t.elapsed());
        assert_eq!((f.width as usize, f.height as usize), (w, h));
        let exp = raw_planes(&want, w, h, w / 2, h, 2);
        assert_eq!(max_diff(&planes(&f), &exp), 0, "frame {n} differs from ffmpeg");
    }
}
