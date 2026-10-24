//! Bit-exactness against ffmpeg's decoder on libx265 / VideoToolbox fixtures (skipped when ffmpeg is
//! absent).

use crate::common;

macro_rules! fixture_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                match common::check_fixture(stringify!($name)) {
                    Ok(true) => {}
                    Ok(false) => eprintln!("skipped {}", stringify!($name)),
                    Err(e) => panic!("{e}"),
                }
            }
        )*
    };
}

fixture_tests!(
    intra_main,
    intra_main10,
    intra_nosao_nodbk,
    ultrafast,
    veryfast,
    medium,
    slow,
    medium_main10,
    slow_main10,
    p_only,
    bframes,
    no_sao,
    no_deblock,
    deblock_m3_3,
    no_wpp,
    rect_amp,
    tskip,
    weightp,
    weightp_main10,
    crf0,
    crf51,
    qp1_main10,
    lossless,
    cu_lossless,
    slices4,
    scaling_list,
    ctu16,
    ctu32_tu4,
    min_cu16,
    no_signhide,
    constrained_intra,
    no_tmvp,
    no_strong_intra,
    max_merge1,
    open_gop,
    keyint5_idr,
    odd_1918x1078,
    hd_1080p,
    uhd_4k_main10,
    vt_main,
    vt_main10,
);

/// Draft mode: sub-layer non-reference pictures skip deblocking and SAO and are flagged; every
/// other picture stays bit-exact, single-threaded and frame-threaded.
#[test]
fn draft_mode_changes_only_flagged_non_reference_pictures() {
    let mut checked = 0;
    for name in ["bframes", "medium", "slow", "weightp_main10"] {
        let f = common::fixture(name);
        let Some((hevc, yuv)) = common::ensure(f) else {
            eprintln!("skipped {name}");
            continue;
        };
        let reference = std::fs::read(&yuv).unwrap();
        let (w, h) = (f.width as usize, f.height as usize);
        let bps = if f.bit_depth > 8 { 2 } else { 1 };
        let fsize = (w * h + 2 * w.div_ceil(2) * h.div_ceil(2)) * bps;
        for threads in [1, 3] {
            let pics = common::decode_file_opts(&hevc, threads, true).unwrap_or_else(|(au, e, _)| panic!("{name}: error at {au}: {e}"));
            common::check_pts(&pics).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(pics.len() * fsize, reference.len(), "{name}: picture count");
            let (mut draft, mut changed) = (0, 0);
            for (i, p) in pics.iter().enumerate() {
                let bytes: Vec<u8> = [&p.y, &p.u, &p.v].iter().flat_map(|pl| pl.to_le_bytes()).collect();
                let same = bytes == reference[i * fsize..(i + 1) * fsize];
                if p.draft {
                    draft += 1;
                    changed += !same as usize;
                } else {
                    assert!(same, "{name} (threads {threads}): picture {i} (not draft) differs from the reference");
                }
            }
            assert!(draft > 0, "{name}: has non-reference pictures");
            assert!(changed > 0, "{name}: skipping the in-loop filters changes some draft picture");
            checked += 1;
        }
    }
    if checked == 0 {
        eprintln!("skipped: no fixtures");
    }
}

/// Print which coding tools each fixture exercises (run with `-- --ignored --nocapture`).
#[test]
#[ignore]
fn coverage_report() {
    for f in common::FIXTURES {
        let Some((hevc, _)) = common::ensure(f) else { continue };
        let data = std::fs::read(hevc).unwrap();
        let mut dec = filmcraft_hevc::Decoder::new();
        for au in common::split_access_units(&data) {
            let _ = dec.decode(au, 0);
        }
        dec.flush();
        println!("{:<20} {:?}", f.name, dec.stats());
    }
}

/// The comparison must report a single flipped sample (guards against a vacuous harness).
#[test]
fn compare_detects_mismatch() {
    let f = common::fixture("intra_main10");
    let Some((hevc, yuv)) = common::ensure(f) else { return };
    let pics = common::decode_file_threads(&hevc, 1).map_err(|e| e.1).unwrap();
    let mut reference = std::fs::read(yuv).unwrap();
    common::compare(&pics, &reference, f.width as usize, f.height as usize, f.bit_depth).unwrap();
    let pos = reference.len() - 7;
    reference[pos] ^= 1;
    let err = common::compare(&pics, &reference, f.width as usize, f.height as usize, f.bit_depth).unwrap_err();
    assert!(err.contains("plane V"), "{err}");
}

/// Pre-generate every fixture and its reference decode (`cargo xtask fixtures`).
#[test]
#[ignore]
fn generate_fixtures() {
    let dir = common::fixtures_dir();
    for f in common::FIXTURES {
        let outs = [dir.join(format!("{}.hevc", f.name)), dir.join(format!("{}.yuv", f.name))];
        filmcraft_testkit::fixtures::generate_and_report(&format!("hevc/{}", f.name), &outs, || common::ensure(f));
    }
}

/// Decoding inside pictures on several threads: wavefront rows (x265's default streams carry
/// WPP entry points, also with several slices per picture) and, without WPP, in-loop filters next
/// to the parsing thread. Every thread count gives exactly the single-threaded pictures, and the
/// pictures were in fact split.
#[test]
fn intra_picture_parallel_matches_single_thread() {
    for (name, wpp) in [("medium", true), ("slices4", true), ("hd_1080p", true), ("no_wpp", false), ("bframes", true)] {
        let f = common::fixture(name);
        let Some((hevc, _)) = common::ensure(f) else {
            eprintln!("skipped {name}");
            continue;
        };
        let (serial, stats) = common::decode_file_cfg(&hevc, 1, false, None).unwrap_or_else(|(au, e, _)| panic!("{name}: error at {au}: {e}"));
        // x265 enables WPP unless told not to (`no-wpp=1`)
        assert_eq!(stats.wpp_pictures > 0, wpp, "{name}: WPP pictures {}", stats.wpp_pictures);
        assert_eq!(stats.parallel_pictures, 0, "{name}: one thread splits no picture");
        for threads in [2, 4, 8] {
            let (pics, stats) = common::decode_file_cfg(&hevc, threads, false, Some(true))
                .unwrap_or_else(|(au, e, _)| panic!("{name} ({threads} threads): error at {au}: {e}"));
            // (without the thread pool everything is decoded on the calling thread)
            let split = if cfg!(feature = "threads") { stats.pictures } else { 0 };
            assert_eq!(stats.parallel_pictures, split, "{name} ({threads} threads): every picture split");
            assert_eq!(pics.len(), serial.len(), "{name} ({threads} threads)");
            for (i, (a, b)) in pics.iter().zip(&serial).enumerate() {
                assert!(a.y == b.y && a.u == b.u && a.v == b.v && a.pts == b.pts, "{name} ({threads} threads): picture {i} differs from one thread");
            }
        }
    }
}
