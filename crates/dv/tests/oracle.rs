//! FFmpeg is an external test oracle only. The decoder has no dependencies or subprocesses.
use filmcraft_dv::decode_frame;
use std::{fs, process::Command};

#[test]
fn generated_patterns_match_reference_and_mutations_never_panic() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("SKIPPED: ffmpeg test oracle unavailable");
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures").join(format!("dv-oracle-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for (i, pattern) in [
        "testsrc2=size=720x480:rate=30000/1001",
        "color=black:size=720x480:rate=30000/1001",
        "color=white:size=720x480:rate=30000/1001",
        "smptebars=size=720x480:rate=30000/1001",
        "nullsrc=size=720x480:rate=30000/1001,geq=lum='128+100*sin(X*0.4)*sin(Y*0.7)':cb='128+60*sin(X*0.2)':cr='128+60*cos(Y*0.3)'",
        "testsrc2=size=720x480:rate=60000/1001,tinterlace=mode=interleave_bottom",
    ]
    .iter()
    .enumerate()
    {
        let dv = dir.join(format!("{i}.dv"));
        let raw = dir.join(format!("{i}.yuv"));
        let result = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                pattern,
                "-frames:v",
                "6",
                "-pix_fmt",
                "yuv411p",
                "-c:v",
                "dvvideo",
                "-threads",
                "1",
                "-an",
                "-f",
                "dv",
            ])
            .arg(&dv)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let result = Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-i"])
            .arg(&dv)
            .args(["-pix_fmt", "yuv411p", "-f", "rawvideo"])
            .arg(&raw)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let input = fs::read(&dv).unwrap();
        let reference = fs::read(&raw).unwrap();
        let mut output = Vec::new();
        for packet in input.as_chunks::<120000>().0 {
            let f = decode_frame(packet).unwrap();
            output.extend(f.y);
            output.extend(f.cb);
            output.extend(f.cr);
        }
        assert_eq!(output.len(), reference.len());
        let max = output.iter().zip(&reference).map(|(&a, &b)| a.abs_diff(b)).max().unwrap();
        let mse = output.iter().zip(&reference).map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2)).sum::<f64>() / output.len() as f64;
        assert!(max <= 3 && mse < 0.5, "pattern {i}: max={max}, mse={mse}");
        // Mutate arbitrary DIF IDs, headers, lengths and VLC payload bits of an actual frame.
        let frame = &input[..120000];
        let mut state = 0x9387ac34u32;
        for _ in 0..96 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let mut corrupted = frame.to_vec();
            let pos = state as usize % corrupted.len();
            corrupted[pos] ^= 1 << (state % 8);
            assert!(std::panic::catch_unwind(|| decode_frame(&corrupted)).is_ok());
        }
        for n in [1, 79, 80, 119999] {
            assert!(decode_frame(&frame[..n]).is_err());
        }
    }
    fs::remove_dir_all(dir).unwrap();
}
