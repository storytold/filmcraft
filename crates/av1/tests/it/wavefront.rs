//! Stress tests of the frame threading (parse / reconstruct split and the row wavefront across
//! frames): every thread count gives the single-thread pictures, run after run, and dropping
//! or flushing a busy decoder never hangs. A hang fails the test through the deadline.

use crate::common;
use common::*;
use filmcraft_av1::{Decoder, Picture};

/// A GOP with many superblock rows (the wavefront has rows to overlap), hierarchical
/// references and motion reaching across rows.
fn spec() -> Spec {
    Spec::new("wavefront_640x480", "testsrc2=s=640x480:r=25,rotate=a=t*0.5:c=black", 24, "yuv420p", &["-preset", "8", "-crf", "38"])
}

/// Seconds a test may take before it counts as a hang (override: FILMCRAFT_HOSTILE_TIMEOUT_SECS).
const DEADLINE_SECS: u64 = 300;

fn decode(tus: &[Vec<u8>], threads: usize) -> Vec<Picture> {
    let mut dec = Decoder::with_threads(threads);
    let mut pics = Vec::new();
    for (i, tu) in tus.iter().enumerate() {
        pics.extend(dec.decode_pts(tu, i as i64).unwrap_or_else(|e| panic!("threads {threads}, unit {i}: {e}")));
    }
    pics.extend(dec.flush_result().unwrap_or_else(|e| panic!("threads {threads}, flush: {e}")));
    pics
}

#[test]
fn every_thread_count_gives_the_single_thread_pictures_repeatedly() {
    let Some(ff) = ffmpeg() else { return };
    let tus = ivf_frames(&make(&ff, &spec()));
    filmcraft_testkit::with_deadline(DEADLINE_SECS, move || {
        let want = decode(&tus, 1);
        assert_eq!(want.len(), 24);
        for threads in [2, 3, 4, 5, 8, 16] {
            for round in 0..4 {
                assert!(decode(&tus, threads) == want, "threads {threads}, round {round}: pictures differ");
            }
        }
    });
}

/// 128x128 superblocks in two tile columns (libaom), with motion field estimation: the
/// wavefront over the superblock rows of every tile column.
fn spec_sb128_tiles() -> Spec {
    let enc = ["-cpu-used", "8", "-crf", "40", "-tile-columns", "1", "-lag-in-frames", "16", "-aom-params", "sb-size=128:enable-ref-frame-mvs=1"];
    Spec::new("wavefront_sb128_tiles", "testsrc2=s=640x400:r=25,rotate=a=t*0.5:c=black", 16, "yuv420p", &enc).encoder("libaom-av1")
}

#[test]
fn superblocks_of_128_in_tile_columns_give_the_single_thread_pictures() {
    let Some(ff) = ffmpeg_libaom() else { return };
    let tus = ivf_frames(&make(&ff, &spec_sb128_tiles()));
    filmcraft_testkit::with_deadline(DEADLINE_SECS, move || {
        let want = decode(&tus, 1);
        assert_eq!(want.len(), 16);
        for threads in [2, 3, 4, 8, 16] {
            for round in 0..2 {
                assert!(decode(&tus, threads) == want, "threads {threads}, round {round}: pictures differ");
            }
        }
    });
}

#[test]
fn dropping_or_flushing_a_busy_decoder_never_hangs() {
    let Some(ff) = ffmpeg() else { return };
    let tus = ivf_frames(&make(&ff, &spec()));
    filmcraft_testkit::with_deadline(DEADLINE_SECS, move || {
        let want = decode(&tus, 1);
        for threads in [2, 3, 8, 16] {
            for cut in [1, 2, 7, 12, 23] {
                // Dropped with frames in flight (no flush).
                let mut dec = Decoder::with_threads(threads);
                for (i, tu) in tus.iter().take(cut).enumerate() {
                    dec.decode_pts(tu, i as i64).expect("decode");
                }
                drop(dec);
                // Flushed mid-stream (as before a seek), then continued: the same pictures.
                let mut dec = Decoder::with_threads(threads);
                let mut pics = Vec::new();
                for (i, tu) in tus.iter().enumerate() {
                    if i == cut {
                        pics.extend(dec.flush_result().expect("flush"));
                    }
                    pics.extend(dec.decode_pts(tu, i as i64).expect("decode"));
                }
                pics.extend(dec.flush_result().expect("flush"));
                assert!(pics == want, "threads {threads}, flush before unit {cut}: pictures differ");
            }
        }
    });
}
