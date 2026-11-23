//! External PCM check for the audible level step and correlated-crossfade bump.
//! Only synthetic tones are used; FFmpeg is a test oracle and is never bundled.
use filmcraft_audio_dsp::remix::{Piece, Plan, match_levels, render_with_levels};
use std::process::Command;

#[test]
fn ffmpeg_measures_a_continuous_level_across_a_correlated_remix_seam() {
    let ffmpeg = filmcraft_testkit::require_ffmpeg!();
    let sr = 48000;
    let plan = Plan { pieces: vec![Piece { src: 0, len: 48000 }, Piece { src: 96000, len: 48000 }], xfade: 960 };
    let mut read = |start: i64, n: usize| {
        vec![
            (0..n)
                .map(|i| {
                    let frame = start + i as i64;
                    let amplitude = if frame >= 72000 { 0.8 } else { 0.2 };
                    (amplitude * (std::f64::consts::TAU * 1000. * frame as f64 / f64::from(sr)).sin()) as f32
                })
                .collect::<Vec<_>>(),
        ]
    };
    let levels = match_levels(&plan, sr, 24000., &mut |a, n| Some(read(a, n))).unwrap();
    assert!(levels.joins[0].correlation > 0.99);
    let audio = render_with_levels(&plan, Some(&levels), 0, 96000, 1, &mut read);
    let bytes: Vec<u8> = audio[0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("filmcraft-remix-seam-{}-{nonce}.f32", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let mut measured = Vec::new();
    for start in [47040, 47760, 48720] {
        let output = Command::new(&ffmpeg)
            .args(["-hide_banner", "-nostats", "-f", "f32le", "-ar", "48000", "-ac", "1", "-i"])
            .arg(&path)
            .args(["-af", &format!("atrim=start_sample={start}:end_sample={},astats=metadata=0:reset=0", start + 480), "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stderr = String::from_utf8_lossy(&output.stderr);
        let rms = stderr.lines().filter_map(|line| line.split_once("RMS level dB: ").and_then(|(_, v)| v.trim().parse::<f64>().ok())).next_back().unwrap();
        measured.push(rms);
    }
    std::fs::remove_file(path).unwrap();
    eprintln!("FFmpeg RMS before/seam/after (dBFS): {measured:?}");
    let expected = 20. * (0.2_f64 / 2_f64.sqrt()).log10();
    for db in &measured {
        assert!((db - expected).abs() < 0.1, "seam RMS {measured:?}, expected {expected}");
    }
    assert!(
        measured.iter().copied().fold(f64::NEG_INFINITY, f64::max) - measured.iter().copied().fold(f64::INFINITY, f64::min) < 0.1,
        "audible step: {measured:?}"
    );
}
