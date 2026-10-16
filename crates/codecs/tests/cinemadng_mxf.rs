//! CinemaDNG in MXF (CinemaDNG Image Data Format Specification 1.0.0.0, §7): synthetic OP1a
//! files from our MXF writer, wrapping DNG frames from the `filmcraft-dng` test writer, one
//! frame-wrapped DNG dataset per edit unit with PCM sound and a start timecode. No tool we can
//! run reads CinemaDNG MXF, so the expected values are the DNGs' own (known) contents.

use std::io::Cursor;
use std::sync::Arc;

use filmcraft_codecs::MxfSource;
use filmcraft_color::Transfer;
use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Options, Value, write};
use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_media::{FrameRequest, MediaKind, MediaSource};
use filmcraft_mxf::{FrameInfo, MxfWriter, PackageIds, Pattern, PictureCoding, PictureDesc, Rational, SoundDesc, StartTimecode, WriterConfig};
use filmcraft_time::FrameRate;

const W: usize = 16;
const H: usize = 12;

/// A 16 × 12 DNG of a flat grey at `level` of the 12-bit range: RGGB lossless JPEG white balanced
/// by neutral (0.5, 1, 0.5), or LinearRaw (three samples per pixel, uncompressed).
fn grey_dng(level: f64, linear_raw: bool) -> Vec<u8> {
    let neutral = [0.5, 1.0, 0.5];
    let mut o = Options {
        width: W,
        height: H,
        bits: 12,
        raw_tags: vec![(tags::WHITE_LEVEL, Value::Short(vec![4095; if linear_raw { 3 } else { 1 }]))],
        main_tags: vec![(tags::AS_SHOT_NEUTRAL, Value::rationals(&neutral))],
        ..Default::default()
    };
    let px: Vec<u16> = if linear_raw {
        o.samples_per_pixel = 3;
        o.cfa = None;
        (0..W * H * 3).map(|i| (level * neutral[i % 3] * 4095.0).round() as u16).collect()
    } else {
        o.compression = Compression::LosslessJpeg { predictor: 1, components: 2, restart_lines: 0, pt: 0 };
        let pat = [0usize, 1, 1, 2];
        (0..W * H).map(|i| (level * neutral[pat[(i / W % 2) * 2 + i % W % 2]] * 4095.0).round() as u16).collect()
    };
    write(&px, &o)
}

fn level(i: usize) -> f64 {
    0.1 + 0.15 * i as f64
}

/// An OP1a CinemaDNG file: `n` frames at 24 fps, stereo 16-bit sound (sample k of channel 0 = k,
/// channel 1 = −k), start timecode 10:00:00:00.
fn cinemadng_mxf(n: usize, linear_raw: bool) -> Vec<u8> {
    let rate = Rational::new(24, 1);
    let mut cfg = WriterConfig::new(Pattern::Op1a, rate, PackageIds::from_seed("cinemadng test", "A001_C001"));
    cfg.picture = Some(PictureDesc::new(PictureCoding::CinemaDng { linear_raw }, W as u32, H as u32));
    cfg.sound = vec![SoundDesc { sample_rate: 48_000, channels: 2, bits: 16 }];
    cfg.timecode = Some(StartTimecode { frames: 10 * 3600 * 24, rate, drop_frame: false });
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    for i in 0..n {
        w.push_picture(&grey_dng(level(i), linear_raw), FrameInfo::intra()).unwrap();
        let mut pcm = Vec::new();
        for k in i * 2000..(i + 1) * 2000 {
            pcm.extend_from_slice(&(k as i16).to_le_bytes());
            pcm.extend_from_slice(&(-(k as i16)).to_le_bytes());
        }
        w.push_sound(0, &pcm).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn red(f: &VideoFrame) -> f32 {
    match &f.data {
        PixelData::RgbaF32(p) => p[(5 * W + 5) * 4],
        _ => panic!("float frames"),
    }
}

#[test]
fn cinemadng_mxf_plays_every_frame() {
    for linear_raw in [false, true] {
        let f = cinemadng_mxf(5, linear_raw);
        let src = MxfSource::open("A001_C001.mxf", f.into()).unwrap();
        let info = src.info();
        assert_eq!(info.kind, MediaKind::Movie);
        assert_eq!(info.container, "MXF OP1a");
        let v = info.video.as_ref().unwrap();
        assert_eq!((v.width, v.height, v.codec.as_str(), v.has_alpha, v.par), (16, 12, "CinemaDNG", false, (1, 1)));
        let pf = if linear_raw { "linear 12-bit raw (uncompressed)" } else { "Bayer 12-bit raw (lossless JPEG)" };
        assert_eq!(v.pixel_format, pf);
        assert_eq!(v.color.transfer, Transfer::Linear);
        assert_eq!(info.frame_rate(), FrameRate::FPS_24);
        assert_eq!(info.duration, FrameRate::FPS_24.tick_of(5));
        assert_eq!(info.start_timecode, Some(10 * 3600 * 24));
        // every frame, in a scrambled order (each is developed on its own)
        for i in [3usize, 0, 4, 1, 2, 3] {
            let frame = src.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i as i64))).unwrap();
            assert_eq!((frame.width, frame.height, frame.color.transfer), (16, 12, Transfer::Linear));
            assert!((red(&frame) - level(i) as f32).abs() < 1e-3, "frame {i}: {}", red(&frame));
        }
        // the sound beside the pictures, sample-exact
        let a = src.audio(1234, 3000, 48_000).unwrap();
        for (j, k) in (1234..4234).enumerate() {
            assert_eq!(a.channels[0][j], k as f32 / 32768.0);
            assert_eq!(a.channels[1][j], -(k as f32) / 32768.0);
        }
    }
}

#[test]
fn cinemadng_mxf_opens_through_the_registered_openers() {
    let f: Arc<[u8]> = cinemadng_mxf(2, false).into();
    let src = filmcraft_media::open_bytes("clip.mxf", f, &filmcraft_codecs::openers()).unwrap();
    assert_eq!(src.info().video.as_ref().unwrap().codec, "CinemaDNG");
    assert!(src.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(1))).is_ok());
}

#[test]
fn broken_dng_frames_are_errors_not_panics() {
    // a picture that is not a DNG: the file opens (sound plays), the picture is unsupported
    let rate = Rational::new(24, 1);
    let mut cfg = WriterConfig::new(Pattern::Op1a, rate, PackageIds::from_seed("bad", "bad"));
    cfg.picture = Some(PictureDesc::new(PictureCoding::CinemaDng { linear_raw: false }, 16, 12));
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    w.push_picture(b"II*\0\x08\0\0\0garbage", FrameInfo::intra()).unwrap();
    let f = w.finish().unwrap().into_inner();
    let src = MxfSource::open("bad.mxf", f.into()).unwrap();
    assert!(src.info().video.as_ref().unwrap().codec.contains("unsupported"));
    assert!(src.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).is_err());
    // nothing to develop: no raw settings
    assert!(src.raw_info().is_none());
    assert!(src.with_raw_settings(&filmcraft_media::raw::RawSettings::default()).is_none());
}

#[test]
fn truncated_and_mutated_files_never_panic() {
    let f = cinemadng_mxf(3, false);
    let try_all = |b: Vec<u8>| {
        let r = std::panic::catch_unwind(|| {
            if let Ok(src) = MxfSource::open("x.mxf", b.into()) {
                for i in 0..4 {
                    let _ = src.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i)));
                }
                let _ = src.audio(0, 5000, 48_000);
            }
        });
        assert!(r.is_ok());
    };
    for n in (0..f.len()).step_by(97) {
        try_all(f[..n].to_vec());
    }
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..400 {
        let mut b = f.clone();
        for _ in 0..4 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = (seed as usize) % b.len();
            b[at] ^= (seed >> 32) as u8 | 1;
        }
        try_all(b);
    }
}

#[test]
fn raw_settings_reach_every_mxf_picture() {
    let src = MxfSource::open("A001_C001.mxf", cinemadng_mxf(3, false).into()).unwrap();
    let raw = src.raw_info().unwrap();
    assert_eq!(raw.settings, filmcraft_media::raw::RawSettings::default());
    let bright = src.with_raw_settings(&filmcraft_media::raw::RawSettings { exposure: 1.0, ..Default::default() }).unwrap();
    assert_eq!(bright.info(), src.info());
    assert_eq!(bright.raw_info().unwrap().settings.exposure, 1.0);
    for i in 0..3 {
        let t = FrameRate::FPS_24.tick_of(i as i64);
        let (a, b) = (red(&src.video_frame(FrameRequest::full(t)).unwrap()), red(&bright.video_frame(FrameRequest::full(t)).unwrap()));
        assert!((b - 2.0 * a).abs() < 1e-3 && (a - level(i) as f32).abs() < 1e-3, "frame {i}: {a} {b}");
        // a reduced-resolution request: never smaller than asked for, same values
        let q = bright.video_frame(FrameRequest { time: t, scale: 0.25 }).unwrap();
        assert!(q.width >= 4 && q.height >= 3 && q.width < 16, "{}x{}", q.width, q.height);
    }
    // the sound is still there
    assert!(bright.audio(0, 100, 48_000).is_ok());
}
