//! Bit-exactness against ffmpeg's decoder on libx265 / VideoToolbox fixtures (skipped when ffmpeg is
//! absent).

mod common;

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
    intra_422,
    intra_422_10,
    intra_422_10_nosao_nodbk,
    medium_422_10,
    slow_422,
    p_only_422_10,
    bframes_422_10,
    no_sao_422_10,
    deblock_m3_3_422_10,
    rect_amp_422_10,
    tskip_422_10,
    weightp_422_10,
    qp_422_10,
    crf51_422_10,
    lossless_422_10,
    ctu32_tu4_422_10,
    ctu16_422_10,
    scaling_list_422_10,
    constrained_intra_422_10,
    slices4_422_10,
    odd_1918x1078_422_10,
    uhd_4k_422_10,
);

/// Draft mode: sub-layer non-reference pictures skip deblocking and SAO and are flagged; every
/// other picture stays bit-exact, single-threaded and frame-threaded.
#[test]
fn draft_mode_changes_only_flagged_non_reference_pictures() {
    let mut checked = 0;
    for name in ["bframes", "medium", "slow", "weightp_main10", "bframes_422_10"] {
        let f = common::fixture(name);
        let Some((hevc, yuv)) = common::ensure(f) else {
            eprintln!("skipped {name}");
            continue;
        };
        let reference = std::fs::read(&yuv).unwrap();
        let fsize = common::frame_size(f);
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
    common::compare(&pics, &reference, f.width as usize, f.height as usize, f.bit_depth, f.chroma).unwrap();
    let pos = reference.len() - 7;
    reference[pos] ^= 1;
    let err = common::compare(&pics, &reference, f.width as usize, f.height as usize, f.bit_depth, f.chroma).unwrap_err();
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
