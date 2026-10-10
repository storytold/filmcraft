//! MXF against ffmpeg: ffmpeg writes the files (OP1a, OP-Atom, D-10) and decodes them as the
//! oracle. Our decode must match frame for frame (H.264 bit-exact, VC-3 ±2, ProRes ±1 LSB), seek
//! to random frames exactly, and give sample-exact PCM.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use filmcraft_codecs::MxfSource;
use filmcraft_media::{FrameRequest, MediaKind, MediaSource};
use filmcraft_time::FrameRate;

const V25: &[&str] = &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25"];
const TONE: &[&str] = &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"];
const NOISE: &[&str] = &["-f", "lavfi", "-i", "anoisesrc=color=pink:sample_rate=48000:amplitude=0.5:seed=7"];

fn spec(name: &str) -> Vec<String> {
    let cat = |p: &[&[&str]]| p.concat().into_iter().map(String::from).collect::<Vec<_>>();
    match name {
        // long-GOP H.264 with B pictures (ffmpeg writes no temporal offsets: order from the POCs)
        "mxf_h264_bf2.mxf" => cat(&[
            V25,
            NOISE,
            &["-t", "2", "-c:v", "libx264", "-bf", "2", "-g", "12", "-pix_fmt", "yuv420p", "-c:a", "pcm_s16le", "-ac", "2", "-timecode", "01:00:00:00"],
        ]),
        "mxf_h264_2997df.mxf" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30000/1001"],
            NOISE,
            &["-t", "1", "-c:v", "libx264", "-bf", "0", "-g", "10", "-pix_fmt", "yuv420p", "-c:a", "pcm_s24le", "-ac", "2", "-timecode", "01:00:00;00"],
        ]),
        "mxf_dnxhr_lb.mxf" => {
            cat(&[V25, TONE, &["-t", "1", "-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p", "-c:a", "pcm_s24le", "-ac", "2"]])
        }
        "mxf_prores.mxf" => cat(&[V25, TONE, &["-t", "1", "-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le", "-c:a", "pcm_s16le"]]),
        "mxf_mpeg2.mxf" => cat(&[V25, NOISE, &["-t", "1", "-c:v", "mpeg2video", "-bf", "2", "-c:a", "pcm_s16le"]]),
        // XDCAM HD422 style: 1080i 4:2:2 long GOP
        "mxf_xdcam_hd422.mxf" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=50,tinterlace=mode=interleave_top,setfield=tff"],
            NOISE,
            &[
                "-t",
                "0.4",
                "-c:v",
                "mpeg2video",
                "-pix_fmt",
                "yuv422p",
                "-flags",
                "+ilme+ildct",
                "-bf",
                "2",
                "-g",
                "12",
                "-b:v",
                "50M",
                "-c:a",
                "pcm_s24le",
                "-ac",
                "2",
            ],
        ]),
        "mxf_atom_dnxhr.mxf" => cat(&[V25, &["-t", "1", "-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p", "-f", "mxf_opatom"]]),
        "mxf_atom_pcm.mxf" => cat(&[NOISE, &["-t", "1", "-c:a", "pcm_s24le", "-ac", "1", "-f", "mxf_opatom"]]),
        "mxf_d10.mxf" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=720x608:rate=25"],
            NOISE,
            &[
                "-t",
                "0.4",
                "-c:v",
                "mpeg2video",
                "-pix_fmt",
                "yuv422p",
                "-minrate",
                "30M",
                "-maxrate",
                "30M",
                "-b:v",
                "30M",
                "-bufsize",
                "1200000",
                "-rc_init_occupancy",
                "1200000",
                "-intra_vlc",
                "1",
                "-non_linear_quant",
                "1",
                "-g",
                "1",
                "-flags",
                "+ildct+low_delay",
                "-dc",
                "10",
                "-ps",
                "1",
                "-qmin",
                "1",
                "-qmax",
                "3",
                "-c:a",
                "pcm_s16le",
                "-ac",
                "4",
                "-f",
                "mxf_d10",
            ],
        ]),
        other => panic!("unknown fixture {other}"),
    }
}

pub const ALL: &[&str] = &[
    "mxf_h264_bf2.mxf",
    "mxf_h264_2997df.mxf",
    "mxf_dnxhr_lb.mxf",
    "mxf_prores.mxf",
    "mxf_mpeg2.mxf",
    "mxf_xdcam_hd422.mxf",
    "mxf_atom_dnxhr.mxf",
    "mxf_atom_pcm.mxf",
    "mxf_d10.mxf",
];

fn make(ff: &Path, name: &str) -> PathBuf {
    let args = spec(name);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    fixture(ff, name, &refs).unwrap_or_else(|| panic!("could not generate {name}"))
}

fn open(p: &Path) -> MxfSource {
    MxfSource::open(p.file_name().unwrap().to_str().unwrap(), bytes(p)).unwrap()
}

/// Number of video packets ffprobe lists.
fn ffprobe_video_packets(file: &Path) -> Option<usize> {
    let fp = filmcraft_testkit::ffprobe()?;
    let o = std::process::Command::new(fp)
        .args(["-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

/// Every frame in order, then `seeks` random frames, against ffmpeg's decode (within `tol`).
fn check_video(ff: &Path, file: &Path, pix_fmt: &str, chroma: (usize, usize), bps: usize, tol: u16, seeks: usize) {
    let src = open(file);
    let v = src.info().video.clone().unwrap();
    let (w, h) = (v.width as usize, v.height as usize);
    let (cw, ch) = (w / chroma.0, h / chroma.1);
    let raw = ffmpeg_frames(ff, file, pix_fmt);
    let fsize = (w * h + 2 * cw * ch) * bps;
    let n = raw.len() / fsize;
    let rate = v.frame_rate;
    assert_eq!(src.info().duration, rate.tick_of(n as i64), "{}: duration = ffmpeg's frame count {n}", file.display());
    if let Some(p) = ffprobe_video_packets(file) {
        assert_eq!(p, n, "packets = frames");
    }
    let frame = |i: usize| raw_planes(&raw[i * fsize..(i + 1) * fsize], w, h, cw, ch, bps);
    let mut worst = 0;
    for i in 0..n {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(i as i64))).unwrap();
        let d = max_diff(&planes(&f), &frame(i));
        worst = worst.max(d);
        assert!(d <= tol, "{}: frame {i} differs by {d}", file.display());
    }
    // random access on a fresh source (no warm cache), out of order
    let src = open(file);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..seeks {
        let i = rng.below(n as u64) as usize;
        let f = src.video_frame(FrameRequest::full(rate.tick_of(i as i64))).unwrap();
        let d = max_diff(&planes(&f), &frame(i));
        assert!(d <= tol, "{}: seek to frame {i} differs by {d}", file.display());
    }
    eprintln!("{}: {n} frames, max diff {worst}", file.display());
}

/// Our audio, as interleaved f32, equals ffmpeg's decode exactly.
fn check_audio_exact(ff: &Path, file: &Path, extra: &[&str]) {
    let src = open(file);
    let a = src.info().audio().cloned().unwrap();
    let ch = a.channels as usize;
    let want = ffmpeg_audio_f32(ff, file, extra);
    let n = want.len() / ch;
    assert!(n > 0);
    let got = src.audio(0, n, a.sample_rate).unwrap();
    for i in 0..n {
        for c in 0..ch {
            assert_eq!(got.channels[c][i], want[i * ch + c], "{}: sample {i} channel {c}", file.display());
        }
    }
    // random windows (sample-exact seeking)
    let mut rng = Rng(77);
    for _ in 0..20 {
        let s = rng.below(n as u64 - 100) as usize;
        let len = 1 + rng.below(3000) as usize;
        let got = src.audio(s as i64, len, a.sample_rate).unwrap();
        for k in 0..len.min(n - s) {
            for c in 0..ch {
                assert_eq!(got.channels[c][k], want[(s + k) * ch + c], "window at {s}+{k}");
            }
        }
    }
    eprintln!("{}: {n} sample frames × {ch} channels exact", file.display());
}

#[test]
fn h264_long_gop_with_b_frames_bit_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "mxf_h264_bf2.mxf");
    let src = open(&f);
    let info = src.info();
    assert_eq!(info.container, "MXF OP1a");
    assert_eq!(info.kind, MediaKind::Movie);
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.frame_rate), (320, 240, FrameRate::FPS_25));
    assert_eq!(v.codec, "H.264");
    assert_eq!(info.start_timecode, Some(90_000), "01:00:00:00 at 25 fps");
    assert_eq!(src.file().timecode.unwrap().format(), "01:00:00:00");
    let t = &src.file().tracks[src.file().track_of_kind(filmcraft_mxf::TrackKind::Picture).unwrap()];
    assert!(t.needs_reorder && !t.temporal_offsets, "ffmpeg marks B pictures without temporal offsets");
    assert_eq!(t.samples.iter().filter(|s| s.key).count(), 5, "an IDR every 12 frames over 50");
    check_video(&ff, &f, "yuv420p", (2, 2), 1, 0, 25);
    check_audio_exact(&ff, &f, &[]);
}

#[test]
fn h264_2997_drop_frame_timecode_and_24_bit_audio() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "mxf_h264_2997df.mxf");
    let src = open(&f);
    let info = src.info();
    assert_eq!(info.video.as_ref().unwrap().frame_rate, FrameRate::FPS_29_97);
    let tc = src.file().timecode.unwrap();
    assert!(tc.drop_frame);
    assert_eq!(tc.format(), "01:00:00;00");
    assert_eq!(info.start_timecode, Some(107_892));
    assert_eq!(info.audio().unwrap().bits_per_sample, Some(24));
    check_video(&ff, &f, "yuv420p", (2, 2), 1, 0, 10);
    check_audio_exact(&ff, &f, &[]);
}

#[test]
fn dnxhr_op1a_within_two_lsb() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "mxf_dnxhr_lb.mxf");
    let src = open(&f);
    assert_eq!(src.info().video.as_ref().unwrap().codec, "DNxHR LB");
    check_video(&ff, &f, "yuv422p", (2, 1), 1, 2, 10);
    check_audio_exact(&ff, &f, &[]);
}

#[test]
fn prores_op1a_within_one_lsb() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "mxf_prores.mxf");
    let src = open(&f);
    assert!(src.info().video.as_ref().unwrap().codec.starts_with("Apple ProRes 422"), "{}", src.info().video.as_ref().unwrap().codec);
    check_video(&ff, &f, "yuv422p10le", (2, 1), 2, 1, 10);
    check_audio_exact(&ff, &f, &[]);
}

/// MPEG-2 frames are compared within the IDCT tolerance of `filmcraft-mpeg2v` (an
/// IEEE 1180-accurate IDCT against ffmpeg's integer one).
const MPEG2_TOL: u16 = 4;

#[test]
fn mpeg2_long_gop_decodes() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "mxf_mpeg2.mxf");
    let src = open(&f);
    let v = src.info().video.as_ref().unwrap();
    assert!(v.codec.starts_with("MPEG-2 Video (Main@"), "{}", v.codec);
    assert_eq!(v.pixel_format, "YUV 4:2:0 8-bit, progressive");
    // temporal offsets: ffmpeg's MPEG-2 index carries them
    let t = &src.file().tracks[src.file().track_of_kind(filmcraft_mxf::TrackKind::Picture).unwrap()];
    assert!(t.temporal_offsets);
    check_video(&ff, &f, "yuv420p", (2, 2), 1, MPEG2_TOL, 12);
    check_audio_exact(&ff, &f, &[]);
}

#[test]
fn xdcam_hd422_decodes() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "mxf_xdcam_hd422.mxf");
    let src = open(&f);
    let v = src.info().video.as_ref().unwrap();
    assert_eq!(v.codec, "MPEG-2 Video (4:2:2@High)");
    assert_eq!(v.pixel_format, "YUV 4:2:2 8-bit, interlaced (upper field first)");
    assert_eq!((v.width, v.height), (1920, 1080));
    check_video(&ff, &f, "yuv422p", (2, 1), 1, MPEG2_TOL, 6);
    check_audio_exact(&ff, &f, &[]);
}

#[test]
fn op_atom_video_and_audio_files() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let v = make(&ff, "mxf_atom_dnxhr.mxf");
    let src = open(&v);
    assert_eq!(src.info().container, "MXF OP-Atom");
    assert!(!src.info().has_audio());
    assert_eq!(src.file().tracks[0].wrapping, filmcraft_mxf::Wrapping::Clip);
    check_video(&ff, &v, "yuv422p", (2, 1), 1, 2, 10);
    let a = make(&ff, "mxf_atom_pcm.mxf");
    let src = open(&a);
    assert_eq!(src.info().kind, MediaKind::AudioOnly);
    assert_eq!(src.info().container, "MXF OP-Atom");
    let ai = src.info().audio().cloned().unwrap();
    assert_eq!((ai.sample_rate, ai.channels, ai.bits_per_sample), (48_000, 1, Some(24)));
    assert_eq!(src.info().duration, filmcraft_time::Tick::from_units(48_000, 48_000));
    check_audio_exact(&ff, &a, &[]);
}

#[test]
fn d10_aes3_elements_sample_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "mxf_d10.mxf");
    let src = open(&f);
    let a = src.info().audio().cloned().unwrap();
    assert_eq!(a.codec, "AES3 PCM");
    assert!(a.channels >= 4, "{}", a.channels);
    let v = src.info().video.as_ref().unwrap();
    assert_eq!(v.codec, "MPEG-2 Video (4:2:2@Main)");
    assert_eq!((v.width, v.height), (720, 608));
    // ffmpeg presents D-10 sound as one multichannel stream too
    check_audio_exact(&ff, &f, &[]);
    // IMX: intra-only 4:2:2 at 608 lines (with the VBI)
    check_video(&ff, &f, "yuv422p", (2, 1), 1, MPEG2_TOL, 4);
}

#[test]
fn truncated_and_corrupt_files_never_panic() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for name in ["mxf_h264_bf2.mxf", "mxf_atom_pcm.mxf", "mxf_prores.mxf"] {
        let f = make(&ff, name);
        let full = std::fs::read(&f).unwrap();
        let mut rng = Rng(0xDEAD_BEEF);
        for k in 1..=16 {
            let cut = full.len() * k / 17;
            let part: std::sync::Arc<[u8]> = full[..cut].to_vec().into();
            if let Ok(s) = MxfSource::open(name, part) {
                let info = s.info().clone();
                if let Some(v) = &info.video {
                    for _ in 0..4 {
                        let t = v.frame_rate.tick_of(rng.below(60) as i64);
                        let _ = s.video_frame(FrameRequest::full(t));
                    }
                }
                if let Some(a) = info.audio() {
                    let _ = s.audio(rng.below(96_000) as i64, 4000, a.sample_rate);
                }
            }
        }
        for _ in 0..30 {
            let mut g = full.clone();
            for _ in 0..8 {
                let at = rng.below(g.len() as u64) as usize;
                g[at] = rng.below(256) as u8;
            }
            if let Ok(s) = MxfSource::open(name, g.into()) {
                if s.info().video.is_some() {
                    for i in [0, 7, 30] {
                        let _ = s.video_frame(FrameRequest::full(s.info().frame_rate().tick_of(i)));
                    }
                }
                if s.info().has_audio() {
                    let _ = s.audio(0, 9600, 48_000);
                }
            }
        }
    }
}

/// `cargo xtask fixtures codecs`: build every MXF fixture.
#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = filmcraft_testkit::ffmpeg() else {
        for n in ALL {
            filmcraft_testkit::fixtures::report(n, filmcraft_testkit::fixtures::Status::Skipped);
        }
        return;
    };
    for n in ALL {
        let out = dir().join(n);
        filmcraft_testkit::fixtures::generate_and_report(n, std::slice::from_ref(&out), || {
            let args = spec(n);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            fixture(&ff, n, &refs)
        });
    }
}
