//! Bit-exactness against ffmpeg's VP9 decoder on libvpx-vp9 fixtures (skipped when ffmpeg or
//! libvpx is absent).

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
    intra_only_keyframes,
    default_good,
    profile2_10bit,
    profile2_12bit,
    profile1_444,
    profile1_422,
    profile1_440,
    profile3_444_10bit,
    profile3_422_12bit,
    lossless,
    lossless_10bit,
    tiles_cols4,
    tiles_rows_cols,
    tiles_rows4,
    altref_hidden,
    altref_10bit,
    altref_tiles,
    frame_parallel,
    error_resilient,
    odd_size,
    tiny_odd,
    narrow_tall,
    row_mt,
    speed0,
    speed1,
    speed2,
    speed3,
    speed4,
    speed5,
    speed6,
    speed7,
    speed8,
    cq_low,
    cq_q4,
    cq_high,
    cq_10bit_low,
    cq_12bit_high,
    aq_segmentation,
    aq_variance,
    aq_complexity,
    sharpness,
    fade_intra_heavy,
    hd_1080p,
    temporal_layers,
);

/// Draft mode: frames no other frame references skip the loop filter and are flagged; every
/// other picture stays bit-exact, single-threaded and with frame threads.
#[test]
fn draft_mode_changes_only_flagged_non_reference_frames() {
    let f = common::fixture("temporal_layers");
    let Some((ivf, yuv)) = common::ensure(f) else { return };
    let reference = std::fs::read(&yuv).unwrap();
    let data = std::fs::read(&ivf).unwrap();
    let fsize = common::raw_size(f.width as usize, f.height as usize, f.pix_fmt);
    for threads in [1, 3] {
        let mut dec = filmcraft_vp9::Decoder::with_threads(threads);
        dec.set_draft(true);
        let mut pics = Vec::new();
        for (i, c) in common::ivf_frames(&data).into_iter().enumerate() {
            pics.extend(dec.decode(c, i as i64).unwrap());
        }
        pics.extend(dec.flush());
        assert_eq!(pics.len(), f.frames as usize, "threads {threads}");
        let (mut flagged, mut changed) = (0, 0);
        for (i, p) in pics.iter().enumerate() {
            let mut bytes = Vec::new();
            for pl in [&p.y, &p.u, &p.v] {
                bytes.extend(pl.to_le_bytes());
            }
            let same = bytes == reference[i * fsize..(i + 1) * fsize];
            if p.draft {
                flagged += 1;
                changed += !same as usize;
            } else {
                assert!(same, "threads {threads}: unflagged frame {i} differs");
            }
        }
        assert!(flagged >= f.frames as usize / 3, "threads {threads}: only {flagged} draft frames");
        assert!(changed > 0, "threads {threads}: draft frames identical to the filtered ones");
    }
}

/// The loop filter runs as a wavefront over superblock rows on the decoder's pool: repeated
/// decodes with several thread counts all match ffmpeg (a race shows up as a mismatch).
#[test]
fn loop_filter_wavefront_repeatable() {
    let f = common::fixture("hd_1080p");
    let Some((ivf, yuv)) = common::ensure(f) else { return };
    let reference = std::fs::read(yuv).unwrap();
    for threads in [2, 5, 16, 16, 16] {
        let pics = common::decode_file_threads(&ivf, threads).map_err(|e| e.1).unwrap();
        common::compare(&pics, &reference, f.pix_fmt).unwrap_or_else(|e| panic!("threads {threads}: {e}"));
    }
}

/// Two streams of different sizes concatenated into one IVF (key frame at the size change).
#[test]
fn resize_concatenated() {
    let a = common::fixture("default_good");
    let b = common::fixture("odd_size");
    let (Some((ia, ya)), Some((ib, yb))) = (common::ensure(a), common::ensure(b)) else { return };
    let da = std::fs::read(ia).unwrap();
    let db = std::fs::read(ib).unwrap();
    let mut frames: Vec<Vec<u8>> = common::ivf_frames(&da).into_iter().map(|f| f.to_vec()).collect();
    frames.extend(common::ivf_frames(&db).into_iter().map(|f| f.to_vec()));
    let path = common::fixtures_dir().join("resize_concat.ivf");
    common::write_ivf(&path, 352, 288, &frames);
    let mut reference = std::fs::read(ya).unwrap();
    reference.extend(std::fs::read(yb).unwrap());
    common::check_files("resize_concat", &path, &reference, "yuv420p").unwrap();
}

/// Print which coding tools each fixture exercises (run with `-- --ignored --nocapture`).
#[test]
#[ignore]
fn coverage_report() {
    for f in common::FIXTURES {
        let Some((ivf, _)) = common::ensure(f) else { continue };
        let data = std::fs::read(ivf).unwrap();
        let mut dec = filmcraft_vp9::Decoder::new();
        for fr in common::ivf_frames(&data) {
            let _ = dec.decode(fr, 0);
        }
        println!("{:<22} {:?}", f.name, dec.stats());
    }
}

/// The comparison must report a single flipped sample (guards against a vacuous harness).
#[test]
fn compare_detects_mismatch() {
    let f = common::fixture("intra_only_keyframes");
    let Some((ivf, yuv)) = common::ensure(f) else { return };
    let pics = common::decode_file_threads(&ivf, 1).map_err(|e| e.1).unwrap();
    let mut reference = std::fs::read(yuv).unwrap();
    common::compare(&pics, &reference, f.pix_fmt).unwrap();
    let pos = reference.len() - 7;
    reference[pos] ^= 1;
    let err = common::compare(&pics, &reference, f.pix_fmt).unwrap_err();
    assert!(err.contains("plane V"), "{err}");
}

/// Key frame detection (random access points) and decoder reset: decoding again from the
/// second key frame after `reset` gives the same pictures as the uninterrupted decode.
#[test]
fn keyframes_and_reset() {
    let f = common::fixture("fade_intra_heavy");
    let Some((ivf, _)) = common::ensure(f) else { return };
    let data = std::fs::read(ivf).unwrap();
    let chunks = common::ivf_frames(&data);
    let keys: Vec<usize> = (0..chunks.len()).filter(|&i| filmcraft_vp9::is_keyframe(chunks[i])).collect();
    assert_eq!(keys.first(), Some(&0));
    assert!(keys.len() >= 2, "key frames {keys:?}");
    let info = filmcraft_vp9::keyframe_info(chunks[0]).unwrap();
    assert_eq!((info.width, info.height, info.bit_depth), (f.width, f.height, 8));
    assert!(info.subsampling_x && info.subsampling_y);
    let mut dec = filmcraft_vp9::Decoder::with_threads(1);
    let mut all = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        all.extend(dec.decode(c, i as i64).unwrap());
    }
    // Garbage in between, then a reset and a restart at the second key frame.
    let _ = dec.decode(&[0x82, 0x49, 0x83, 0x42, 0x00], 0);
    dec.reset();
    let k = keys[1];
    for (i, c) in chunks.iter().enumerate().skip(k) {
        for p in dec.decode(c, i as i64).unwrap() {
            let want = all.iter().find(|q| q.pts == p.pts).unwrap();
            assert!(p.y == want.y && p.u == want.u && p.v == want.v, "picture {} differs after reset", p.pts);
        }
    }
}
