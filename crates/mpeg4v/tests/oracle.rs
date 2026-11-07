//! MPEG-4 Part 2 video against ffmpeg: ffmpeg encodes lavfi patterns into elementary streams, and
//! ffmpeg's decode of the same file is the reference. The standard leaves the IDCT's rounding open
//! (any IEEE 1180-accurate IDCT conforms), so frames are compared by PSNR; frame count and display
//! order are exact.

mod common;

use common::*;

const MOVING: &str = "testsrc2=size=352x288:rate=25,noise=alls=6:allf=t";

const SPECS: &[Spec] = &[
    Spec { name: "sp_default", lavfi: MOVING, frames: 300, enc: &["-vtag", "XVID", "-q:v", "4", "-g", "300"] },
    Spec { name: "sp_gop12", lavfi: MOVING, frames: 120, enc: &["-vtag", "XVID", "-b:v", "1M", "-g", "12"] },
    Spec { name: "sp_q1", lavfi: MOVING, frames: 60, enc: &["-q:v", "1", "-g", "30"] },
    Spec { name: "sp_q2_escapes", lavfi: "testsrc2=size=352x288:rate=25,noise=alls=40:allf=t", frames: 30, enc: &["-q:v", "2", "-g", "10"] },
    Spec { name: "sp_q31", lavfi: MOVING, frames: 120, enc: &["-q:v", "31", "-g", "60"] },
    Spec { name: "sp_mbd2_trellis", lavfi: MOVING, frames: 120, enc: &["-mbd", "2", "-trellis", "1", "-q:v", "5"] },
    Spec { name: "sp_4mv", lavfi: MOVING, frames: 120, enc: &["-flags", "+mv4", "-mbd", "2", "-q:v", "4"] },
    Spec { name: "sp_acpred", lavfi: MOVING, frames: 60, enc: &["-flags", "+aic", "-mbd", "2", "-g", "5", "-q:v", "6"] },
    Spec { name: "sp_mpeg_quant", lavfi: MOVING, frames: 120, enc: &["-mpeg_quant", "1", "-q:v", "4"] },
    Spec {
        name: "sp_custom_matrix",
        lavfi: MOVING,
        frames: 60,
        enc: &[
            "-mpeg_quant",
            "1",
            "-q:v",
            "3",
            "-intra_matrix",
            "8,9,10,11,12,13,14,15,9,10,11,12,13,14,15,16,10,11,12,13,14,15,16,17,11,12,13,14,15,16,17,18,12,13,14,15,16,17,18,19,13,14,15,16,17,18,19,20,14,15,16,17,18,19,20,21,15,16,17,18,19,20,21,22",
            "-inter_matrix",
            "16,16,17,17,18,18,19,19,16,17,17,18,18,19,19,20,17,17,18,18,19,19,20,20,17,18,18,19,19,20,20,21,18,18,19,19,20,20,21,21,18,19,19,20,20,21,21,22,19,19,20,20,21,21,22,22,19,20,20,21,21,22,22,23",
        ],
    },
    Spec { name: "sp_packets", lavfi: MOVING, frames: 120, enc: &["-ps", "600", "-q:v", "4", "-g", "30"] },
    Spec { name: "sp_partitioned", lavfi: MOVING, frames: 120, enc: &["-data_partitioning", "1", "-ps", "800", "-q:v", "4", "-g", "30"] },
    Spec {
        name: "sp_partitioned_4mv",
        lavfi: MOVING,
        frames: 60,
        enc: &["-data_partitioning", "1", "-ps", "400", "-flags", "+mv4", "-mbd", "2", "-q:v", "6", "-g", "8"],
    },
    Spec { name: "sp_odd_size", lavfi: "testsrc2=size=322x243:rate=25,noise=alls=6:allf=t", frames: 60, enc: &["-q:v", "4", "-pix_fmt", "yuv420p"] },
    Spec {
        name: "sp_fast_motion",
        lavfi: "testsrc2=size=352x288:rate=25,scroll=h=0.09:v=0.05,noise=alls=4:allf=t",
        frames: 90,
        enc: &["-q:v", "4", "-me_range", "64"],
    },
    Spec { name: "asp_b1", lavfi: MOVING, frames: 150, enc: &["-vtag", "DX50", "-bf", "1", "-q:v", "4", "-g", "60"] },
    Spec { name: "asp_b2", lavfi: MOVING, frames: 300, enc: &["-vtag", "XVID", "-bf", "2", "-q:v", "4", "-g", "300"] },
    Spec { name: "asp_b2_direct_rich", lavfi: MOVING, frames: 150, enc: &["-bf", "2", "-mbd", "2", "-flags", "+mv4", "-q:v", "5", "-g", "24"] },
    Spec { name: "asp_b2_mpeg_quant", lavfi: MOVING, frames: 90, enc: &["-bf", "2", "-mpeg_quant", "1", "-q:v", "4"] },
    Spec { name: "asp_b2_packets", lavfi: MOVING, frames: 90, enc: &["-bf", "2", "-ps", "600", "-q:v", "4"] },
    Spec { name: "asp_b2_partitioned", lavfi: MOVING, frames: 90, enc: &["-bf", "2", "-data_partitioning", "1", "-ps", "800", "-q:v", "4"] },
    Spec { name: "asp_qpel", lavfi: MOVING, frames: 150, enc: &["-flags", "+qpel", "-q:v", "4"] },
    Spec { name: "asp_qpel_b2_4mv", lavfi: MOVING, frames: 150, enc: &["-flags", "+qpel+mv4", "-bf", "2", "-mbd", "2", "-q:v", "4", "-g", "48"] },
];

#[test]
fn matches_ffmpeg_frame_by_frame() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let only = std::env::var("MPEG4V_ONLY").ok();
    let mut failed = Vec::new();
    println!("{:<22} {:>6} {:>8} {:>8} {:>8} {:>5}", "fixture", "frames", "min dB", "mean dB", "last dB", "max±");
    for spec in SPECS {
        if only.as_deref().is_some_and(|o| !spec.name.contains(o)) {
            continue;
        }
        let file = make(&ff, spec).expect("fixture");
        let es = std::fs::read(&file).unwrap();
        let reference = ffmpeg_frames(&ff, &file);
        let pics = decode_all(&chunks(&es));
        let c = compare(&pics, &reference);
        println!("{:<22} {:>6} {:>8.2} {:>8.2} {:>8.2} {:>5}", spec.name, c.frames, c.min_psnr, c.mean_psnr, c.last_psnr, c.worst);
        if c.min_psnr < 45.0 {
            failed.push((spec.name, c.min_psnr, c.first_diff));
        }
    }
    assert!(failed.is_empty(), "below 45 dB: {failed:?}");
}

/// Encoder streams with damage: packets, partitions, quarter samples, matrices. It never panics.
#[test]
fn damaged_streams_never_panic() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n.max(1)
        }
    }
    let mut r = Rng(0x7C3A_91E5_B24D_60F1);
    for name in ["sp_packets", "sp_partitioned", "sp_partitioned_4mv", "sp_custom_matrix", "asp_b2_packets", "asp_qpel_b2_4mv"] {
        let spec = SPECS.iter().find(|s| s.name == name).unwrap();
        let es = std::fs::read(make(&ff, spec).expect("fixture")).unwrap();
        let clean: Vec<&[u8]> = chunks(&es).into_iter().take(8).collect();
        for round in 0..200 {
            let mut d = filmcraft_mpeg4v::Decoder::new();
            for (i, c) in clean.iter().enumerate() {
                let mut data = c.to_vec();
                match round % 3 {
                    0 if r.below(3) == 0 => data.truncate(r.below(data.len() as u64 + 1) as usize),
                    1 => {
                        for _ in 0..1 + r.below(12) {
                            let at = r.below(data.len() as u64) as usize;
                            data[at] ^= 1 << r.below(8);
                        }
                    }
                    2 => {
                        for _ in 0..1 + r.below(6) {
                            let at = r.below(data.len() as u64) as usize;
                            data[at] = r.below(256) as u8;
                        }
                    }
                    _ => {}
                }
                let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.decode(&data, i as i64).map(|p| p.len())));
                assert!(got.is_ok(), "{name}: round {round}, chunk {i}");
            }
            let _ = d.flush();
        }
    }
}
