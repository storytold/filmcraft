//! MPEG transport / program streams against ffmpeg: ffmpeg writes the files and decodes them as
//! the oracle. Video: frame count and display order exact, PTS exact against ffprobe, H.264 /
//! HEVC bit-exact, MPEG-2 within the IDCT tolerance of `filmcraft-mpeg2v` (±4, PSNR ≥ 58 dB);
//! random seeks give the frames of the sequential decode. Audio: LPCM sample-exact, MPEG audio
//! and AAC within a float-decoder tolerance, starting at the same sample.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use filmcraft_codecs::MpegSource;
use filmcraft_media::{FrameRequest, MediaError, MediaSource};

/// Stereo test audio with different content per channel (tones and a chirp).
const NOISE: &[&str] =
    &["-f", "lavfi", "-i", "aevalsrc=exprs=0.3*sin(2*PI*441*t)+0.1*sin(2*PI*3100*t)|0.25*sin(2*PI*660*t)+0.1*sin(2*PI*(500+4000*t)*t):s=48000"];

fn spec(name: &str) -> Vec<String> {
    let cat = |p: &[&[&str]]| p.concat().into_iter().map(String::from).collect::<Vec<_>>();
    match name {
        // broadcast-style SD: interlaced MPEG-2 + MPEG-1 layer II
        "ts_mpeg2_mp2.ts" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=720x576:rate=50,tinterlace=mode=interleave_top,setfield=tff"],
            NOISE,
            &["-t", "1.2", "-c:v", "mpeg2video", "-flags", "+ilme+ildct", "-bf", "2", "-g", "12", "-b:v", "6M", "-c:a", "mp2", "-b:a", "192k", "-f", "mpegts"],
        ]),
        "ts_h264_aac.ts" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25"],
            NOISE,
            &["-t", "1.2", "-c:v", "libx264", "-bf", "2", "-g", "12", "-pix_fmt", "yuv420p", "-c:a", "aac", "-aac_pns", "0", "-b:a", "128k", "-f", "mpegts"],
        ]),
        // AVCHD-style BDAV (192-byte packets): H.264 + Blu-ray LPCM
        "m2ts_h264_lpcm.m2ts" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30000/1001"],
            NOISE,
            &[
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-bf",
                "2",
                "-g",
                "15",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "pcm_bluray",
                "-ac",
                "2",
                "-f",
                "mpegts",
                "-mpegts_m2ts_mode",
                "1",
            ],
        ]),
        // AVCHD audio is usually AC-3
        "m2ts_h264_ac3.mts" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30000/1001"],
            NOISE,
            &[
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-bf",
                "2",
                "-g",
                "15",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "ac3",
                "-b:a",
                "192k",
                "-ac",
                "2",
                "-f",
                "mpegts",
                "-mpegts_m2ts_mode",
                "1",
            ],
        ]),
        "ts_hevc_latm.ts" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25"],
            NOISE,
            &[
                "-t",
                "1",
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error:bframes=2:keyint=12",
                "-c:a",
                "aac",
                "-aac_pns",
                "0",
                "-b:a",
                "96k",
                "-f",
                "mpegts",
                "-mpegts_flags",
                "latm",
            ],
        ]),
        // DVD-style program streams
        "ps_mpeg2_mp2.vob" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=720x480:rate=30000/1001"],
            NOISE,
            &["-t", "1.2", "-c:v", "mpeg2video", "-bf", "2", "-g", "15", "-b:v", "5M", "-c:a", "mp2", "-b:a", "224k", "-f", "vob"],
        ]),
        "ps_mpeg2_lpcm.vob" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=720x576:rate=25"],
            NOISE,
            &["-t", "1", "-c:v", "mpeg2video", "-bf", "2", "-c:a", "pcm_dvd", "-ac", "2", "-f", "vob"],
        ]),
        "ps_mpeg2_lpcm16.vob" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25"],
            NOISE,
            &["-t", "0.6", "-c:v", "mpeg2video", "-c:a", "pcm_dvd", "-sample_fmt", "s16", "-ac", "2", "-f", "vob"],
        ]),
        "ps_mpeg2_ac3.vob" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=720x576:rate=25"],
            NOISE,
            &["-t", "1", "-c:v", "mpeg2video", "-bf", "2", "-c:a", "ac3", "-b:a", "192k", "-f", "vob"],
        ]),
        // ISO/IEC 11172-1 system stream
        "mpeg1_system.mpg" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25"],
            NOISE,
            &["-t", "1.2", "-c:v", "mpeg1video", "-bf", "2", "-b:v", "1500k", "-c:a", "mp2", "-b:a", "128k", "-f", "mpeg"],
        ]),
        // XDCAM HD422 in QuickTime (`xd5c`), MPEG-2 in MP4 (`mp4v`, esds object type 0x61) and
        // Matroska (`V_MPEG2`)
        "mov_xdcam_hd422.mov" => cat(&[
            &["-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=50,tinterlace=mode=interleave_top,setfield=tff"],
            &["-t", "0.4", "-c:v", "mpeg2video", "-pix_fmt", "yuv422p", "-flags", "+ilme+ildct", "-bf", "2", "-g", "12", "-b:v", "50M"],
        ]),
        "mp4_mpeg2.mp4" => cat(&[&["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25"], &["-t", "0.6", "-c:v", "mpeg2video", "-bf", "2", "-g", "6"]]),
        "mkv_mpeg2.mkv" => {
            cat(&[&["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25"], NOISE, &["-t", "0.6", "-c:v", "mpeg2video", "-bf", "2", "-g", "6", "-c:a", "mp2"]])
        }
        "audio.mp2" => cat(&[NOISE, &["-t", "1", "-c:a", "mp2", "-b:a", "224k", "-f", "mp2"]]),
        "es_mpeg2.m2v" => {
            cat(&[&["-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25"], &["-t", "1", "-c:v", "mpeg2video", "-bf", "2", "-g", "9", "-f", "mpeg2video"]])
        }
        other => panic!("unknown fixture {other}"),
    }
}

pub const ALL: &[&str] = &[
    "ts_mpeg2_mp2.ts",
    "ts_h264_aac.ts",
    "m2ts_h264_lpcm.m2ts",
    "m2ts_h264_ac3.mts",
    "ts_hevc_latm.ts",
    "ps_mpeg2_mp2.vob",
    "ps_mpeg2_lpcm.vob",
    "ps_mpeg2_lpcm16.vob",
    "ps_mpeg2_ac3.vob",
    "mpeg1_system.mpg",
    "mov_xdcam_hd422.mov",
    "mp4_mpeg2.mp4",
    "mkv_mpeg2.mkv",
    "audio.mp2",
    "es_mpeg2.m2v",
];

fn make(ff: &Path, name: &str) -> PathBuf {
    let args = spec(name);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    fixture(ff, name, &refs).unwrap_or_else(|| panic!("could not generate {name}"))
}

fn open(p: &Path) -> MpegSource {
    MpegSource::open(p.file_name().unwrap().to_str().unwrap(), bytes(p)).unwrap()
}

/// ffprobe's video frames in display order: PTS (90 kHz). A program-stream picture
/// need not carry its own PTS; use the decoder's presentation timestamp for that picture.
fn ffprobe_frame_pts(file: &Path) -> Option<Vec<i64>> {
    let fp = filmcraft_testkit::ffprobe_or_skip("MPEG video frame presentation timestamps")?;
    let o = std::process::Command::new(fp)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pts,best_effort_timestamp", "-of", "json"])
        .arg(file)
        .output()
        .unwrap_or_else(|error| panic!("{}: cannot run ffprobe: {error}", file.display()));
    assert!(o.status.success(), "{}: ffprobe failed: {}", file.display(), String::from_utf8_lossy(&o.stderr));
    Some(parse_ffprobe_frame_pts(&o.stdout).unwrap_or_else(|error| panic!("{}: {error}", file.display())))
}

fn parse_ffprobe_frame_pts(bytes: &[u8]) -> Result<Vec<i64>, String> {
    let data: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| format!("invalid ffprobe JSON: {error}"))?;
    let frames = data.get("frames").and_then(serde_json::Value::as_array).ok_or("ffprobe output is missing its frames array")?;
    if frames.is_empty() {
        return Err("ffprobe returned no video frames".into());
    }
    frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            let field = if frame.get("pts").is_some() { "pts" } else { "best_effort_timestamp" };
            frame.get(field).and_then(serde_json::Value::as_i64).ok_or_else(|| format!("ffprobe video frame {index} is missing an integer {field}"))
        })
        .collect()
}

#[test]
fn ffprobe_pts_preserve_missing_raw_timestamps_and_side_data() {
    let output = br#"{"frames":[
        {"pts":131400,"best_effort_timestamp":131401,"side_data_list":[{},{}]},
        {"best_effort_timestamp":135000,"side_data_list":[{}]},
        {"pts":138600,"best_effort_timestamp":138600}
    ]}"#;
    assert_eq!(parse_ffprobe_frame_pts(output).unwrap(), [131400, 135000, 138600], "keep every display frame, with raw PTS preferred when present");
    assert_eq!(parse_ffprobe_frame_pts(br#"{"frames":[{"pts":-3600},{"pts":0},{"pts":9223372036854775807}]}"#).unwrap(), [-3600, 0, i64::MAX]);
}

#[test]
fn malformed_ffprobe_pts_fail_instead_of_dropping_frames() {
    for output in [
        "not JSON",
        "{}",
        r#"{"frames":{}}"#,
        r#"{"frames":[]}"#,
        r#"{"frames":[{}]}"#,
        r#"{"frames":[null]}"#,
        r#"{"frames":[{"pts":"N/A","best_effort_timestamp":135000}]}"#,
        r#"{"frames":[{"pts":null,"best_effort_timestamp":135000}]}"#,
        r#"{"frames":[{"pts":1.5}]}"#,
        r#"{"frames":[{"pts":9223372036854775808}]}"#,
        r#"{"frames":[{"best_effort_timestamp":"135000"}]}"#,
        r#"{"frames":[{"pts":131400},{"best_effort_timestamp":null},{"pts":138600}]}"#,
    ] {
        assert!(parse_ffprobe_frame_pts(output.as_bytes()).is_err(), "malformed oracle output must fail: {output}");
    }
}

/// Decode every frame in order and `seeks` random frames; compare with ffmpeg within `tol`.
/// Returns (max difference, PSNR).
fn check_video(ff: &Path, file: &Path, tol: u16, seeks: usize) -> (u16, f64) {
    let src = open(file);
    let v = src.info().video.clone().unwrap();
    let (w, h) = (v.width as usize, v.height as usize);
    let first = src.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
    let (cw, ch, pix) = match &first.data {
        filmcraft_frame::PixelData::Yuv8 { chroma: filmcraft_frame::Chroma::C422, .. } => (w / 2, h, "yuv422p"),
        _ => (w.div_ceil(2), h.div_ceil(2), "yuv420p"),
    };
    let raw = ffmpeg_frames(ff, file, pix);
    let fsize = w * h + 2 * cw * ch;
    let n = raw.len() / fsize;
    let rate = v.frame_rate;
    assert_eq!(src.info().duration, rate.tick_of(n as i64), "{}: duration = ffmpeg's {n} frames", file.display());
    let frame = |i: usize| raw_planes(&raw[i * fsize..(i + 1) * fsize], w, h, cw, ch, 1);
    let (mut worst, mut sse, mut cnt) = (0u16, 0f64, 0u64);
    let mut ours = Vec::new();
    for i in 0..n {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(i as i64))).unwrap();
        let p = planes(&f);
        let r = frame(i);
        let d = max_diff(&p, &r);
        for (a, b) in p.iter().zip(&r) {
            for (x, y) in a.iter().zip(b) {
                sse += (*x as f64 - *y as f64).powi(2);
                cnt += 1;
            }
        }
        worst = worst.max(d);
        assert!(d <= tol, "{}: frame {i} differs by {d}", file.display());
        ours.push(p);
    }
    // random access on a fresh source gives the same frames
    let fresh = open(file);
    let mut rng = Rng(0x5EED);
    for _ in 0..seeks {
        let i = rng.below(n as u64) as usize;
        let f = fresh.video_frame(FrameRequest::full(rate.tick_of(i as i64))).unwrap();
        assert!(planes(&f) == ours[i], "{}: seek to frame {i} differs from the sequential decode", file.display());
    }
    // presentation timestamps in display order = ffprobe's
    if let Some(fp) = ffprobe_frame_pts(file) {
        let pts = src.video_pts();
        let missing: Vec<usize> = (0..pts.len()).filter(|&i| pts[i].is_none()).collect();
        assert!(missing.is_empty(), "{}: frames without PTS: {missing:?} (ffprobe {:?})", file.display(), &fp[..fp.len().min(12)]);
        let pts: Vec<i64> = pts.into_iter().flatten().collect();
        assert_eq!(pts, fp, "{}: frame PTS", file.display());
    }
    let psnr = if sse == 0.0 { f64::INFINITY } else { 10.0 * (255f64 * 255.0 * cnt as f64 / sse).log10() };
    (worst, psnr)
}

/// Our decoded audio from the first audio frame against ffmpeg's decode. Returns the max error.
fn check_audio(ff: &Path, file: &Path, tol: f32) -> f32 {
    let src = open(file);
    let a = src.info().audio().cloned().unwrap();
    let (start, len) = src.audio_extent().unwrap();
    let want = ffmpeg_audio_f32(ff, file, &[]);
    let ch = a.channels as usize;
    assert_eq!(want.len() % ch, 0);
    let n = want.len() / ch;
    assert_eq!(len as usize, n, "{}: audio samples = ffmpeg's", file.display());
    let buf = src.audio(start, n, a.sample_rate).unwrap();
    let mut worst = 0f32;
    for i in 0..n {
        for c in 0..ch {
            worst = worst.max((buf.channels[c][i] - want[i * ch + c]).abs());
        }
    }
    if worst > tol {
        // where it starts, and the lag that best aligns the two (diagnostics)
        let first_bad = (0..n * ch).find(|&k| (buf.channels[k % ch][k / ch] - want[k]).abs() > tol).map(|k| k / ch);
        let best = (-4000i64..=4000)
            .map(|lag| {
                let e: f64 = (4000..n.min(20_000))
                    .map(|i| {
                        let j = i as i64 + lag;
                        if j < 0 || j as usize >= n { 0.0 } else { (buf.channels[0][i] as f64 - want[j as usize * ch] as f64).powi(2) }
                    })
                    .sum();
                (e, lag)
            })
            .fold((f64::MAX, 0), |a, b| if b.0 < a.0 { b } else { a });
        let per: Vec<String> = (0..n.min(16 * 1152))
            .step_by(1152)
            .map(|s| {
                let e = (s..(s + 1152).min(n)).map(|i| (buf.channels[0][i] - want[i * ch]).abs()).fold(0f32, f32::max);
                let ours = (s..(s + 1152).min(n)).map(|i| buf.channels[0][i].abs()).fold(0f32, f32::max);
                format!("{e:.3}/{ours:.3}")
            })
            .collect();
        panic!(
            "{}: audio max error {worst}, first at sample {first_bad:?}, best lag {} (residual {:.3e}); per 1152 (err/peak): {per:?}",
            file.display(),
            best.1,
            best.0
        );
    }
    // a window read on a fresh source (random access) gives the same samples
    let fresh = open(file);
    let at = start + n as i64 / 3;
    let win = fresh.audio(at, 4800.min(n / 2), a.sample_rate).unwrap();
    for c in 0..ch {
        for (k, v) in win.channels[c].iter().enumerate() {
            let i = (at - start) as usize + k;
            assert!((v - buf.channels[c][i]).abs() <= 1e-6, "{}: random-access audio differs at {i}", file.display());
        }
    }
    worst
}

#[test]
fn transport_stream_mpeg2_and_mp2() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ts_mpeg2_mp2.ts");
    let src = open(&f);
    let v = src.info().video.clone().unwrap();
    assert_eq!(src.info().container, "MPEG-2 TS");
    assert!(v.codec.starts_with("MPEG-2 Video (Main@"), "{}", v.codec);
    assert_eq!(v.pixel_format, "YUV 4:2:0 8-bit, interlaced (upper field first)");
    assert_eq!((v.width, v.height, v.par), (720, 576, (1, 1)));
    let (worst, psnr) = check_video(&ff, &f, 4, 12);
    assert!(psnr >= 58.0, "PSNR {psnr}");
    println!("ts_mpeg2_mp2: video max diff {worst}, PSNR {psnr:.2} dB");
    let a = src.info().audio().cloned().unwrap();
    assert_eq!((a.codec.as_str(), a.sample_rate, a.channels), ("MPEG Audio", 48_000, 2));
    let e = check_audio(&ff, &f, 2e-4);
    println!("ts_mpeg2_mp2: MP2 audio max error {e:.2e}");
}

#[test]
fn transport_stream_h264_and_aac() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ts_h264_aac.ts");
    let src = open(&f);
    assert_eq!(src.info().video.as_ref().unwrap().codec, "H.264");
    check_video(&ff, &f, 0, 10);
    let e = check_audio(&ff, &f, 1e-4);
    println!("ts_h264_aac: AAC max error {e:.2e}");
}

#[test]
fn bdav_h264_with_lpcm_sample_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "m2ts_h264_lpcm.m2ts");
    let src = open(&f);
    assert_eq!(src.info().container, "MPEG-2 TS (BDAV/AVCHD)");
    let a = src.info().audio().cloned().unwrap();
    assert_eq!((a.codec.as_str(), a.channels, a.bits_per_sample), ("LPCM (Blu-ray)", 2, Some(24)));
    check_video(&ff, &f, 0, 10);
    assert_eq!(check_audio(&ff, &f, 0.0), 0.0);
}

#[test]
fn avchd_ac3_audio() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "m2ts_h264_ac3.mts");
    let src = open(&f);
    check_video(&ff, &f, 0, 6);
    let a = src.info().audio().cloned().unwrap();
    if filmcraft_codecs::audio::AC3_DECODER {
        assert_eq!((a.codec.as_str(), a.channels), ("AC-3", 2));
        // AC-3 zero-bit mantissas carry decoder-specific dither (A/52 §7.3.4)
        let e = check_audio(&ff, &f, 5e-3);
        println!("m2ts_h264_ac3: AC-3 max error {e:.2e}");
    } else {
        assert_eq!(a.codec, "AC-3 (unsupported)");
        match src.audio(0, 100, 48_000) {
            Err(MediaError::Unsupported(why)) => assert!(why.contains("AC-3"), "{why}"),
            other => panic!("expected unsupported, got {:?}", other.map(|_| ())),
        }
    }
}

#[test]
fn transport_stream_hevc_and_latm() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ts_hevc_latm.ts");
    let src = open(&f);
    assert_eq!(src.info().video.as_ref().unwrap().codec, "HEVC");
    assert_eq!(src.info().audio().unwrap().codec, "AAC (LATM)");
    check_video(&ff, &f, 0, 8);
    let e = check_audio(&ff, &f, 1e-4);
    println!("ts_hevc_latm: AAC (LATM) max error {e:.2e}");
}

#[test]
fn program_streams() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ps_mpeg2_mp2.vob");
    let src = open(&f);
    assert_eq!(src.info().container, "MPEG-2 PS");
    let (worst, psnr) = check_video(&ff, &f, 4, 10);
    assert!(psnr >= 58.0);
    println!("ps_mpeg2_mp2: video max diff {worst}, PSNR {psnr:.2} dB");
    check_audio(&ff, &f, 2e-4);
    // DVD LPCM: sample-exact
    let f = make(&ff, "ps_mpeg2_lpcm.vob");
    let src = open(&f);
    assert_eq!(src.info().audio().unwrap().codec, "LPCM (DVD)");
    assert_eq!(src.info().audio().unwrap().bits_per_sample, Some(24));
    assert_eq!(check_audio(&ff, &f, 0.0), 0.0);
    check_video(&ff, &f, 4, 4);
    let f = make(&ff, "ps_mpeg2_lpcm16.vob");
    assert_eq!(open(&f).info().audio().unwrap().bits_per_sample, Some(16));
    assert_eq!(check_audio(&ff, &f, 0.0), 0.0);
    // AC-3 in private stream 1
    let f = make(&ff, "ps_mpeg2_ac3.vob");
    let src = open(&f);
    assert_eq!(src.info().audio().unwrap().codec, "AC-3");
    let e = check_audio(&ff, &f, 5e-3);
    println!("ps_mpeg2_ac3: AC-3 max error {e:.2e}");
    // MPEG-1 system stream
    let f = make(&ff, "mpeg1_system.mpg");
    let src = open(&f);
    assert_eq!(src.info().container, "MPEG-1 System");
    assert_eq!(src.info().video.as_ref().unwrap().codec, "MPEG-1 Video");
    let (worst, psnr) = check_video(&ff, &f, 4, 6);
    assert!(psnr >= 58.0);
    println!("mpeg1_system: video max diff {worst}, PSNR {psnr:.2} dB");
    check_audio(&ff, &f, 2e-4);
}

#[test]
fn video_elementary_stream() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "es_mpeg2.m2v");
    let src = filmcraft_codecs::open_bytes("es_mpeg2.m2v", bytes(&f)).unwrap();
    assert_eq!(src.info().container, "MPEG video elementary stream");
    let raw = ffmpeg_frames(&ff, &f, "yuv420p");
    let fsize = 352 * 288 * 3 / 2;
    let n = raw.len() / fsize;
    let rate = src.info().frame_rate();
    assert_eq!(src.info().duration, rate.tick_of(n as i64));
    for i in 0..n {
        let fr = src.video_frame(FrameRequest::full(rate.tick_of(i as i64))).unwrap();
        let d = max_diff(&planes(&fr), &raw_planes(&raw[i * fsize..(i + 1) * fsize], 352, 288, 176, 144, 1));
        assert!(d <= 4, "frame {i}: {d}");
    }
}

#[test]
fn mpeg2_in_quicktime_mp4_and_matroska() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, chroma) in [("mov_xdcam_hd422.mov", (2, 1)), ("mp4_mpeg2.mp4", (2, 2)), ("mkv_mpeg2.mkv", (2, 2))] {
        let f = make(&ff, name);
        let src = filmcraft_codecs::open_bytes(name, bytes(&f)).unwrap();
        let v = src.info().video.clone().unwrap();
        let (w, h) = (v.width as usize, v.height as usize);
        let (cw, ch) = (w / chroma.0, h / chroma.1);
        let raw = ffmpeg_frames(&ff, &f, if chroma.1 == 1 { "yuv422p" } else { "yuv420p" });
        let fsize = w * h + 2 * cw * ch;
        let n = raw.len() / fsize;
        assert!(n >= 10, "{name}: {n} frames");
        for i in 0..n {
            let fr = src.video_frame(FrameRequest::full(v.frame_rate.tick_of(i as i64))).unwrap();
            let d = max_diff(&planes(&fr), &raw_planes(&raw[i * fsize..(i + 1) * fsize], w, h, cw, ch, 1));
            assert!(d <= 4, "{name}: frame {i} differs by {d}");
        }
    }
}

#[test]
fn standalone_mp2_file() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "audio.mp2");
    let src = filmcraft_codecs::open_bytes("audio.mp2", bytes(&f)).unwrap();
    let a = src.info().audio().cloned().unwrap();
    assert_eq!((a.sample_rate, a.channels), (48_000, 2));
    let want = ffmpeg_audio_f32(&ff, &f, &[]);
    let n = want.len() / 2;
    let buf = src.audio(0, n, 48_000).unwrap();
    let worst = (0..n).map(|i| (buf.channels[0][i] - want[2 * i]).abs().max((buf.channels[1][i] - want[2 * i + 1]).abs())).fold(0f32, f32::max);
    assert!(worst <= 2e-4, "{worst}");
}

#[test]
fn truncated_and_corrupt_files_never_panic() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for name in ["ts_mpeg2_mp2.ts", "ps_mpeg2_mp2.vob", "ts_h264_aac.ts"] {
        let f = make(&ff, name);
        let full = std::fs::read(&f).unwrap();
        let mut rng = Rng(0xC0FFEE);
        let exercise = |data: Vec<u8>, rng: &mut Rng| {
            let Ok(s) = MpegSource::open(name, data.into()) else { return };
            let info = s.info().clone();
            if let Some(v) = &info.video {
                let frames = v.frame_rate.frame_at(info.duration).max(1);
                for _ in 0..3 {
                    let i = rng.below(frames as u64) as i64;
                    let _ = s.video_frame(FrameRequest::full(v.frame_rate.tick_of(i)));
                }
            }
            if let Some(a) = info.audio() {
                let _ = s.audio(rng.below(48_000) as i64, 2048, a.sample_rate);
            }
        };
        for k in 1..=8 {
            exercise(full[..full.len() * k / 9].to_vec(), &mut rng);
        }
        for _ in 0..12 {
            let mut m = full.clone();
            for _ in 0..40 {
                let i = rng.below(m.len() as u64) as usize;
                m[i] = rng.below(256) as u8;
            }
            exercise(m, &mut rng);
        }
    }
}

#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("codecs mpeg fixtures") else { return };
    for name in ALL {
        let out = [dir().join(name)];
        filmcraft_testkit::fixtures::generate_and_report(&format!("codecs/{name}"), &out, || {
            let args = spec(name);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            fixture(&ff, name, &refs)
        });
    }
}
