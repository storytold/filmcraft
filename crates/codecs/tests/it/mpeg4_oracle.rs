//! MPEG-4 Part 2 in AVI against ffmpeg: ffmpeg writes the files (or our small muxer does, for what
//! ffmpeg cannot write: the packed bitstream, a layer header only in `strf`) and decodes them as the
//! oracle. Frame by frame by PSNR, as the IDCT's rounding is left open by the standard; random seeks
//! give the frames of the sequential decode exactly.

use std::path::Path;

use crate::common::*;
use filmcraft_media::{FrameRequest, SharedSource};

const MOVING: &str = "testsrc2=size=352x288:rate=25,noise=alls=6:allf=t";

fn open(path: &Path) -> SharedSource {
    filmcraft_codecs::open_bytes("clip.avi", bytes(path)).expect("open")
}

fn ours(src: &SharedSource, k: usize) -> Vec<u8> {
    let rate = src.info().video.as_ref().unwrap().frame_rate;
    let f = src.video_frame(FrameRequest::full(rate.tick_of(k as i64))).unwrap_or_else(|e| panic!("frame {k}: {e}"));
    planes(&f).concat().into_iter().map(|v| v as u8).collect()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sse: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y)).pow(2)).sum();
    if sse == 0 { 99.0 } else { 10.0 * (65025.0 * a.len() as f64 / sse as f64).log10() }
}

/// Every frame in order, then `seeks` random ones, against ffmpeg's frames; the worst PSNR.
fn check(name: &str, path: &Path, reference: &[u8], frames: usize, seeks: usize) -> f64 {
    let src = open(path);
    let size = reference.len() / frames;
    let mut worst = f64::MAX;
    let mut decoded = Vec::with_capacity(frames);
    for (k, r) in reference.chunks_exact(size).enumerate() {
        let got = ours(&src, k);
        let db = psnr(&got, r);
        assert!(db > 45.0, "{name}: frame {k} of {frames} at {db:.1} dB");
        worst = worst.min(db);
        decoded.push(got);
    }
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for _ in 0..seeks {
        let src = open(path);
        let k = rng.below(frames as u64) as usize;
        assert!(ours(&src, k) == decoded[k], "{name}: frame {k} after a seek differs from the sequential decode");
    }
    worst
}

#[test]
fn avi_files_match_ffmpeg_frame_by_frame_and_seek() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let specs: &[(&str, &[&str], usize)] = &[
        ("mp4v_xvid_ip.avi", &["-vtag", "XVID", "-q:v", "4", "-g", "24"], 120),
        ("mp4v_dx50_ip.avi", &["-vtag", "DX50", "-b:v", "800k", "-g", "50"], 120),
        ("mp4v_xvid_b1.avi", &["-vtag", "XVID", "-bf", "1", "-q:v", "4", "-g", "24"], 120),
        ("mp4v_xvid_b2.avi", &["-vtag", "XVID", "-bf", "2", "-q:v", "4", "-g", "24"], 120),
        ("mp4v_dx50_b2_open.avi", &["-vtag", "DX50", "-bf", "2", "-q:v", "4", "-g", "12", "-flags", "-cgop"], 120),
        ("mp4v_xvid_qpel_b2.avi", &["-vtag", "XVID", "-flags", "+qpel+mv4", "-bf", "2", "-mbd", "2", "-q:v", "4", "-g", "24"], 90),
        ("mp4v_global_header.avi", &["-vtag", "XVID", "-flags", "+global_header", "-bf", "1", "-q:v", "4", "-g", "24"], 60),
    ];
    for (name, enc, frames) in specs {
        let n = frames.to_string();
        let mut args = vec!["-f", "lavfi", "-i", MOVING, "-frames:v", &n, "-c:v", "mpeg4"];
        args.extend_from_slice(enc);
        let path = fixture(&ff, name, &args).expect("fixture");
        let reference = ffmpeg_frames(&ff, &path, "yuv420p");
        assert_eq!(reference.len() % (352 * 288 * 3 / 2), 0);
        assert_eq!(reference.len() / (352 * 288 * 3 / 2), *frames, "{name}: ffmpeg's frame count");
        let db = check(name, &path, &reference, *frames, 12);
        println!("{name:<28} {frames:>5} frames, worst {db:.2} dB");
    }
}

#[test]
fn info_of_an_avi() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = fixture(
        &ff,
        "mp4v_info.avi",
        &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25", "-frames:v", "10", "-c:v", "mpeg4", "-vtag", "XVID", "-aspect", "16:9"],
    )
    .expect("fixture");
    let src = open(&path);
    let v = src.info().video.clone().unwrap();
    assert_eq!((v.width, v.height, v.codec.as_str()), (320, 240, "MPEG-4 Part 2 (DivX / Xvid)"));
    assert_eq!(v.par, (4, 3));
    assert_eq!(v.pixel_format, "YUV 4:2:0 8-bit, progressive");
}

/// The other MPEG-4 of AVI files: a different bitstream, named for what it is.
#[test]
fn ms_mpeg4_is_named_and_reported() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, codec, tag, label) in [("ms_div3.avi", "msmpeg4", "DIV3", "MS-MPEG4 (DivX 3)"), ("ms_mp42.avi", "msmpeg4v2", "MP42", "MS-MPEG4 v2")] {
        let path = fixture(&ff, name, &["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25", "-frames:v", "5", "-c:v", codec, "-vtag", tag]).expect("fixture");
        let src = open(&path);
        assert_eq!(src.info().video.as_ref().unwrap().codec, label);
        let e = src.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap_err().to_string();
        assert!(e.contains(label), "{e}");
    }
}

/// Features the decoder lacks are errors that name them, not wrong pictures.
#[test]
fn interlaced_coding_is_reported_by_name() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = fixture(
        &ff,
        "mp4v_interlaced.avi",
        &["-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25", "-frames:v", "5", "-c:v", "mpeg4", "-vtag", "XVID", "-flags", "+ildct+ilme"],
    )
    .expect("fixture");
    let e = open(&path).video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap_err().to_string();
    assert!(e.contains("interlaced"), "{e}");
}

/// A small AVI muxer for what ffmpeg will not write.
fn write_avi(chunks: &[(Vec<u8>, bool)], w: u32, h: u32, extra: &[u8], index: bool) -> Vec<u8> {
    fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut o = b"LIST".to_vec();
        o.extend(((body.len() + 4) as u32).to_le_bytes());
        o.extend(kind);
        o.extend(body);
        o
    }
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut o = id.to_vec();
        o.extend((body.len() as u32).to_le_bytes());
        o.extend(body);
        if body.len() % 2 == 1 {
            o.push(0);
        }
        o
    }
    let le = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let n = chunks.len() as u32;
    // avih: µs per frame, max rate, padding, flags (has index), frames, initial, streams, buffer, size
    let avih = le(&[40_000, 0, 0, 0x10, n, 0, 1, 1 << 20, w, h, 0, 0, 0, 0]);
    // strh: vids, handler, flags, priority/language, initial, scale, rate, start, length, buffer...
    let mut strh = b"vidsXVID".to_vec();
    strh.extend(le(&[0, 0, 0, 1, 25, 0, n, 1 << 20, u32::MAX, 0, 0, 0]));
    let mut strf = le(&[40 + extra.len() as u32, w, h]);
    strf.extend(1u16.to_le_bytes());
    strf.extend(24u16.to_le_bytes());
    strf.extend(b"XVID");
    strf.extend(le(&[w * h * 3 / 2, 0, 0, 0, 0]));
    strf.extend(extra);
    let strl = [chunk(b"strh", &strh), chunk(b"strf", &strf)].concat();
    let hdrl = [chunk(b"avih", &avih), list(b"strl", &strl)].concat();
    let mut movi = Vec::new();
    let mut idx = Vec::new();
    for (data, key) in chunks {
        idx.extend(b"00dc");
        idx.extend(le(&[if *key { 0x10 } else { 0 }, movi.len() as u32 + 4, data.len() as u32]));
        movi.extend(chunk(b"00dc", data));
    }
    let mut body = b"AVI ".to_vec();
    body.extend(list(b"hdrl", &hdrl));
    body.extend(list(b"movi", &movi));
    if index {
        body.extend(chunk(b"idx1", &idx));
    }
    let mut o = b"RIFF".to_vec();
    o.extend((body.len() as u32).to_le_bytes());
    o.extend(body);
    o
}

/// VOPs of an elementary stream, each with the headers before it.
fn vops(es: &[u8]) -> Vec<Vec<u8>> {
    let starts: Vec<usize> = filmcraft_mpeg4v::start_codes(es).iter().filter(|c| c.1 == 0xB6).map(|c| c.0).collect();
    let mut out = Vec::new();
    let mut from = 0;
    for (k, &s) in starts.iter().enumerate() {
        // up to the headers before the next VOP
        let end = starts.get(k + 1).map_or(es.len(), |&n| {
            let codes = filmcraft_mpeg4v::start_codes(&es[s..n]);
            codes.get(1).map_or(n, |c| s + c.0)
        });
        out.push(es[from..end].to_vec());
        from = end;
    }
    out
}

fn first_type(chunk: &[u8]) -> filmcraft_mpeg4v::VopType {
    filmcraft_mpeg4v::vop_types(chunk)[0]
}

/// A not-coded P-VOP for a layer with `vop_time_increment_resolution` 25 (5 bits), stuffed.
fn not_coded_vop() -> Vec<u8> {
    // 01 type, 0 modulo, 1 marker, 00000 increment, 1 marker, 0 not coded, 0111 stuffing
    let bits: u32 = 0b0101_0000_0100_1111;
    let mut o = vec![0, 0, 1, 0xB6];
    o.extend(bits.to_be_bytes()[2..].iter());
    o
}

/// Rewrite stream-order VOPs as the packed bitstream: the first B-VOP after an anchor in the
/// anchor's chunk, the others alone, and a not-coded VOP in the slot of the anchor.
fn pack(vops: &[Vec<u8>]) -> Vec<(Vec<u8>, bool)> {
    use filmcraft_mpeg4v::VopType::{B, I};
    let mut out = Vec::new();
    let mut k = 0;
    while k < vops.len() {
        let anchor = &vops[k];
        let key = first_type(anchor) == I;
        k += 1;
        let mut chunk = anchor.clone();
        if vops.get(k).is_some_and(|v| first_type(v) == B) {
            chunk.extend_from_slice(&vops[k]);
            k += 1;
            out.push((chunk, key));
            while vops.get(k).is_some_and(|v| first_type(v) == B) {
                out.push((vops[k].clone(), false));
                k += 1;
            }
            out.push((not_coded_vop(), false));
        } else {
            out.push((chunk, key));
        }
    }
    out
}

fn elementary(ff: &Path, name: &str, enc: &[&str], frames: usize) -> Vec<u8> {
    let n = frames.to_string();
    let mut args = vec!["-f", "lavfi", "-i", MOVING, "-frames:v", &n, "-c:v", "mpeg4"];
    args.extend_from_slice(enc);
    args.extend_from_slice(&["-f", "m4v"]);
    std::fs::read(fixture(ff, name, &args).expect("fixture")).unwrap()
}

/// The packed bitstream: one picture per slot, in display order, whatever the seek, and the same
/// pictures as the stream-order file (ffmpeg's decode of which is the reference).
#[test]
fn packed_bitstream_shows_one_picture_per_slot() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (name, enc) in [
        ("packed_b1", &["-bf", "1", "-q:v", "4", "-g", "24"][..]),
        ("packed_b2", &["-bf", "2", "-q:v", "4", "-g", "24"][..]),
        ("packed_b2_open", &["-bf", "2", "-q:v", "4", "-g", "12", "-flags", "-cgop"][..]),
    ] {
        let frames = 90;
        let es = elementary(&ff, &format!("{name}.m4v"), enc, frames);
        let reference = ffmpeg_frames(&ff, &dir().join(format!("{name}.m4v")), "yuv420p");
        let mut chunks = pack(&vops(&es));
        assert_eq!(chunks.len(), frames, "{name}: one chunk per slot");
        // the user data DivX writes: what tells ffmpeg (not us) that the stream is packed
        let first_vop = filmcraft_mpeg4v::start_codes(&chunks[0].0).iter().find(|c| c.1 == 0xB6).unwrap().0;
        chunks[0].0.splice(first_vop..first_vop, b"\0\0\x01\xB2DivX503b1393p".iter().copied());
        assert!(chunks.iter().any(|c| filmcraft_mpeg4v::vop_types(&c.0).len() == 2));
        let path = dir().join(format!("{name}.avi"));
        std::fs::write(&path, write_avi(&chunks, 352, 288, &[], true)).unwrap();
        let db = check(name, &path, &reference, frames, 24);
        println!("{name:<28} {frames:>5} frames, worst {db:.2} dB");
        // ffmpeg's own reading of the packed file shows the same pictures
        let theirs = ffmpeg_frames(&ff, &path, "yuv420p");
        assert_eq!(theirs.len(), reference.len(), "{name}: ffmpeg's frames of the packed file");
        assert!(theirs == reference, "{name}: packed file differs for ffmpeg");
    }
}

/// The layer header only in the stream format: the chunks have none.
#[test]
fn layer_header_in_extradata_only() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let frames = 48;
    let name = "extradata.m4v";
    let es = elementary(&ff, name, &["-q:v", "4", "-g", "12"], frames);
    let reference = ffmpeg_frames(&ff, &dir().join(name), "yuv420p");
    let codes = filmcraft_mpeg4v::start_codes(&es);
    let first_vop = codes.iter().find(|c| c.1 == 0xB6).unwrap().0;
    let extra = es[..first_vop].to_vec();
    assert!(filmcraft_mpeg4v::probe(&extra).is_some());
    let strip = |v: &[u8]| -> Vec<u8> {
        let codes = filmcraft_mpeg4v::start_codes(v);
        v[codes.iter().find(|c| c.1 == 0xB6).unwrap().0..].to_vec()
    };
    let chunks: Vec<(Vec<u8>, bool)> = vops(&es).iter().map(|v| (strip(v), first_type(v) == filmcraft_mpeg4v::VopType::I)).collect();
    assert!(chunks.iter().all(|c| filmcraft_mpeg4v::probe(&c.0).is_none()));
    let path = dir().join("extradata.avi");
    std::fs::write(&path, write_avi(&chunks, 352, 288, &extra, true)).unwrap();
    let db = check("extradata", &path, &reference, frames, 12);
    println!("extradata only               {frames:>5} frames, worst {db:.2} dB");
}

/// No index: the key frames are found in the chunks.
#[test]
fn key_frames_without_an_index() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let frames = 48;
    let name = "noindex.m4v";
    let es = elementary(&ff, name, &["-bf", "2", "-q:v", "4", "-g", "12"], frames);
    let reference = ffmpeg_frames(&ff, &dir().join(name), "yuv420p");
    let chunks: Vec<(Vec<u8>, bool)> = vops(&es).into_iter().map(|v| (v, false)).collect();
    let path = dir().join("noindex.avi");
    std::fs::write(&path, write_avi(&chunks, 352, 288, &[], false)).unwrap();
    let db = check("no index", &path, &reference, frames, 12);
    println!("no index                     {frames:>5} frames, worst {db:.2} dB");
}

/// `points` seek points, `run` frames from each, every one against ffmpeg's frames from there.
fn seek_runs(ff: &Path, path: &Path, points: &[usize], run: usize) {
    let reader = std::sync::Arc::new(filmcraft_media::reader::FileReader::open(path).expect("open file"));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let src = filmcraft_codecs::AviSource::open_reader(&name, reader).expect("open");
    let src: SharedSource = std::sync::Arc::new(src);
    let v = src.info().video.clone().unwrap();
    println!("{}x{} {} at {:.3} fps", v.width, v.height, v.codec, v.frame_rate.as_f64());
    let size = (v.width * v.height) as usize + 2 * (v.width.div_ceil(2) * v.height.div_ceil(2)) as usize;
    for &at in points {
        let t = format!("{:.6}", (at as f64 - 0.25).max(0.0) / v.frame_rate.as_f64());
        let n = run.to_string();
        let path = path.to_string_lossy().into_owned();
        let args = ["-ss", &t, "-i", &path, "-map", "0:v:0", "-frames:v", &n, "-fps_mode", "passthrough", "-pix_fmt", "yuv420p", "-f", "rawvideo", "-"];
        let reference = ffmpeg_out(ff, &args);
        assert_eq!(reference.len(), run * size, "ffmpeg's frames from {at}");
        let (mut worst, mut sum) = (f64::MAX, 0.0);
        let t0 = std::time::Instant::now();
        for (k, r) in reference.chunks_exact(size).enumerate() {
            let db = psnr(&ours(&src, at + k), r);
            worst = worst.min(db);
            sum += db;
        }
        println!(
            "from frame {at:>6}: {run} frames, min {worst:.2} dB, mean {:.2} dB, {:.0} fps through the source",
            sum / run as f64,
            run as f64 / t0.elapsed().as_secs_f64()
        );
        assert!(worst > 45.0, "from frame {at}: {worst:.2} dB");
    }
}

/// A long GOP at the size of a TV episode, entered at several points.
#[test]
fn long_gops_entered_at_several_points() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = fixture(
        &ff,
        "mp4v_720x404.avi",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=720x404:rate=24000/1001,noise=alls=6:allf=t",
            "-frames:v",
            "1200",
            "-c:v",
            "mpeg4",
            "-vtag",
            "XVID",
            "-b:v",
            "1500k",
            "-g",
            "250",
        ],
    )
    .expect("fixture");
    seek_runs(&ff, &path, &[0, 480, 1100], 60);
}

/// A real file (`MPEG4_SAMPLE`, `MPEG4_POINTS` a comma-separated list of frames, `MPEG4_RUN`
/// frames from each). Files people own stay out of the repository.
#[test]
#[ignore]
fn real_file() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Ok(path) = std::env::var("MPEG4_SAMPLE") else {
        println!("MPEG4_SAMPLE not set");
        return;
    };
    let points: Vec<usize> =
        std::env::var("MPEG4_POINTS").unwrap_or_else(|_| "0,3000,12000,30000,50000,65000".into()).split(',').filter_map(|p| p.parse().ok()).collect();
    let run = std::env::var("MPEG4_RUN").ok().and_then(|r| r.parse().ok()).unwrap_or(500);
    seek_runs(&ff, Path::new(&path), &points, run);
}

/// Corrupt files: decoding goes on or reports, and never panics.
#[test]
fn damaged_avi_never_panics() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = fixture(
        &ff,
        "mp4v_fuzz.avi",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=96x80:rate=25,noise=alls=10:allf=t",
            "-frames:v",
            "24",
            "-c:v",
            "mpeg4",
            "-vtag",
            "XVID",
            "-bf",
            "2",
            "-q:v",
            "6",
            "-g",
            "8",
        ],
    )
    .expect("fixture");
    let clean = std::fs::read(&path).unwrap();
    let mut rng = Rng(0x7A5F_11C3_9D2E_4B61);
    for round in 0..120 {
        let mut b = clean.clone();
        match round % 3 {
            0 => b.truncate(rng.below(b.len() as u64 + 1) as usize),
            1 => {
                for _ in 0..1 + rng.below(40) {
                    let at = rng.below(b.len() as u64) as usize;
                    b[at] ^= 1 << rng.below(8);
                }
            }
            _ => {
                for _ in 0..1 + rng.below(12) {
                    let at = rng.below(b.len() as u64) as usize;
                    b[at] = rng.below(256) as u8;
                }
            }
        }
        let got = std::panic::catch_unwind(|| {
            if let Ok(src) = filmcraft_codecs::open_bytes("damaged.avi", b.into()) {
                let Some(rate) = src.info().video.as_ref().map(|v| v.frame_rate) else { return };
                for k in [0i64, 5, 23, 11] {
                    let _ = src.video_frame(FrameRequest::full(rate.tick_of(k)));
                }
            }
        });
        assert!(got.is_ok(), "round {round}");
    }
}
