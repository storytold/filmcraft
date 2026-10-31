//! MPEG-1/2 video against ffmpeg: ffmpeg encodes lavfi test patterns into elementary streams,
//! ffmpeg's decode of the same file is the reference. MPEG-2's IDCT is not bit-exact by
//! definition (any IEEE 1180-accurate IDCT conforms), so frames are compared within a tolerance;
//! frame count and display order are exact.

use crate::common;

use common::*;

const SPECS: &[Spec] = &[
    // progressive 4:2:0 IBBP, default VLCs
    Spec {
        name: "m2v_cif_ibbp",
        lavfi: "testsrc2=size=352x288:rate=25",
        frames: 30,
        enc: &["-c:v", "mpeg2video", "-bf", "2", "-g", "12", "-b:v", "3M"],
        pix_fmt: "yuv420p",
    },
    // interlaced (frame pictures, field MC + field DCT), top field first, alternate scan
    Spec {
        name: "m2v_576i_tff",
        lavfi: "testsrc2=size=720x576:rate=50,tinterlace=mode=interleave_top,setfield=tff",
        frames: 16,
        enc: &["-c:v", "mpeg2video", "-flags", "+ilme+ildct", "-alternate_scan", "1", "-bf", "2", "-g", "8", "-b:v", "6M"],
        pix_fmt: "yuv420p",
    },
    // bottom field first, intra VLC table one, non-linear quantiser, 10-bit DC
    Spec {
        name: "m2v_480i_bff_vlc1",
        lavfi: "testsrc2=size=720x480:rate=60000/1001,tinterlace=mode=interleave_bottom,setfield=bff",
        frames: 12,
        enc: &[
            "-c:v",
            "mpeg2video",
            "-flags",
            "+ilme+ildct",
            "-intra_vlc",
            "1",
            "-non_linear_quant",
            "1",
            "-qmax",
            "28",
            "-dc",
            "10",
            "-bf",
            "1",
            "-g",
            "6",
            "-b:v",
            "8M",
        ],
        pix_fmt: "yuv420p",
    },
    // 4:2:2 intra-only, IMX / D-10 style (720x608, 9-bit... DC precision 10, low delay)
    Spec {
        name: "m2v_imx_422_intra",
        lavfi: "testsrc2=size=720x608:rate=25",
        frames: 6,
        enc: &[
            "-c:v",
            "mpeg2video",
            "-pix_fmt",
            "yuv422p",
            "-g",
            "1",
            "-intra_vlc",
            "1",
            "-non_linear_quant",
            "1",
            "-dc",
            "10",
            "-flags",
            "+ildct+low_delay",
            "-qmin",
            "1",
            "-qmax",
            "3",
            "-b:v",
            "30M",
            "-minrate",
            "30M",
            "-maxrate",
            "30M",
            "-bufsize",
            "1200000",
        ],
        pix_fmt: "yuv422p",
    },
    // 4:2:2 long GOP interlaced HD (XDCAM HD422 style)
    Spec {
        name: "m2v_hd422_longgop",
        lavfi: "testsrc2=size=1920x1080:rate=50,tinterlace=mode=interleave_top,setfield=tff",
        frames: 8,
        enc: &["-c:v", "mpeg2video", "-pix_fmt", "yuv422p", "-flags", "+ilme+ildct", "-bf", "2", "-g", "6", "-b:v", "50M"],
        pix_fmt: "yuv422p",
    },
    // low quantiser: large levels (escape codes), custom quantiser matrices
    Spec {
        name: "m2v_escapes_qmatrix",
        lavfi: "mandelbrot=size=320x240:rate=25",
        frames: 10,
        enc: &[
            "-c:v",
            "mpeg2video",
            "-qscale:v",
            "1",
            "-qmin",
            "1",
            "-bf",
            "2",
            "-g",
            "5",
            "-intra_matrix",
            "8,16,16,16,17,18,21,24,16,16,16,16,17,19,22,25,16,16,17,18,20,22,25,29,16,16,18,21,24,27,31,36,17,17,20,24,30,35,41,47,18,19,22,27,35,44,54,65,21,22,25,31,41,54,70,88,24,25,29,36,47,65,88,115",
            "-inter_matrix",
            "18,18,18,18,19,21,23,27,18,18,18,18,19,21,24,29,18,18,19,20,22,24,28,32,18,18,20,24,27,30,35,40,19,19,22,27,33,39,46,53,21,21,24,30,39,50,61,73,23,24,28,35,46,61,79,98,27,29,32,40,53,73,98,129",
        ],
        pix_fmt: "yuv420p",
    },
    // no B pictures
    Spec { name: "m2v_ipp", lavfi: "testsrc2=size=176x144:rate=30", frames: 20, enc: &["-c:v", "mpeg2video", "-bf", "0", "-g", "10"], pix_fmt: "yuv420p" },
    // MPEG-1 IBBP, and MPEG-1 at q 1 (8/16-bit escapes)
    Spec {
        name: "m1v_sif",
        lavfi: "testsrc2=size=352x240:rate=30000/1001",
        frames: 24,
        enc: &["-c:v", "mpeg1video", "-bf", "2", "-g", "12", "-b:v", "1500k"],
        pix_fmt: "yuv420p",
    },
    Spec {
        name: "m1v_q1",
        lavfi: "mandelbrot=size=192x144:rate=25",
        frames: 8,
        enc: &["-c:v", "mpeg1video", "-qscale:v", "1", "-qmin", "1", "-bf", "1", "-g", "4"],
        pix_fmt: "yuv420p",
    },
];

/// Tolerance against ffmpeg (whose integer IDCT differs from our IEEE 1180-accurate one, and the
/// difference propagates through prediction): per frame max |Δ| ≤ 4 and PSNR ≥ 58 dB; overall
/// at most 3 % of samples differ at all.
pub const TOL_MAX: u8 = 4;
pub const MIN_PSNR: f64 = 58.0;
pub const MAX_DIFFERING: f64 = 0.03;

/// Per displayed frame: (pict_type, interlaced_frame, top_field_first) from ffprobe.
fn ffprobe_frames(file: &std::path::Path) -> Option<Vec<(String, bool, bool)>> {
    let fp = filmcraft_testkit::ffprobe()?;
    let o = std::process::Command::new(fp)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pict_type,interlaced_frame,top_field_first", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let v: Vec<&str> = l.split(',').collect();
                (v[0].to_string(), v.get(1) == Some(&"1"), v.get(2) == Some(&"1"))
            })
            .collect(),
    )
}

fn check(spec: &Spec) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, spec).unwrap_or_else(|| panic!("could not generate {}", spec.name));
    let es = std::fs::read(&f).unwrap();
    let (pics, dec) = decode_es(&es, true);
    assert_eq!(dec.errors(), 0, "{}: slice errors ({:?})", spec.name, dec.last_error());
    let raw = ffmpeg_frames(&ff, &f, spec.pix_fmt);
    let stats = compare(&pics, &raw);
    let mut all = Stats::default();
    for (i, s) in stats.iter().enumerate() {
        all.merge(s);
        assert!(s.max <= TOL_MAX && s.psnr() >= MIN_PSNR, "{} frame {i} ({:?}): max {} psnr {:.2}", spec.name, pics[i].picture_type, s.max, s.psnr());
    }
    assert!((all.diff as f64) / (all.n as f64) <= MAX_DIFFERING, "{}: {} of {} samples differ", spec.name, all.diff, all.n);
    // display order: picture types, interlacing and field order as ffprobe reports them
    if let Some(fr) = ffprobe_frames(&f) {
        assert_eq!(fr.len(), pics.len());
        for (i, (p, (t, il, tff))) in pics.iter().zip(&fr).enumerate() {
            assert_eq!(format!("{:?}", p.picture_type), *t, "{} frame {i} type", spec.name);
            assert_eq!(!p.progressive_frame, *il, "{} frame {i} interlaced", spec.name);
            if *il {
                assert_eq!(p.top_field_first, *tff, "{} frame {i} field order", spec.name);
            }
        }
    }
    println!(
        "{}: {} frames, {} ({}), max diff {}, {:.4}% samples differ, PSNR {:.2} dB",
        spec.name,
        pics.len(),
        pics[0].info.codec_name(),
        pix_fmt_of(pics[0].chroma),
        all.max,
        100.0 * all.diff as f64 / all.n as f64,
        all.psnr()
    );
    // the single-threaded decode is identical
    let (single, _) = decode_es(&es, false);
    assert!(single.iter().zip(&pics).all(|(a, b)| a.y == b.y && a.cb == b.cb && a.cr == b.cr));
}

#[test]
fn ffmpeg_fixtures_match() {
    for s in SPECS {
        check(s);
    }
}

#[test]
fn hd_1080i_matches() {
    check(&HD1080I);
}

#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("mpeg2v fixtures") else { return };
    for s in SPECS.iter().chain([&HD1080I]) {
        filmcraft_testkit::fixtures::generate_and_report(&format!("mpeg2v/{}", s.name), &[path(s)], || make(&ff, s));
    }
}
