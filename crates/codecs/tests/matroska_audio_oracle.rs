//! Matroska audio made by ffmpeg, against ffmpeg's decode (#236). ffmpeg stamps blocks to the
//! millisecond, so at 44.1 kHz AAC (1024-sample), MP3 (1152) and AC-3 (1536) packets are each
//! stamped up to half a millisecond off their exact start. Read back block by block, as playback
//! reads it, a tone must stay as smooth as ffmpeg's decode of it: no holes or overlaps between
//! packets.

mod common;

use common::*;

/// Largest |second difference| of `x` from `from` on: a click shows up as a spike.
fn roughness(x: &[f32], from: usize) -> f32 {
    x.windows(3).skip(from).map(|w| (w[0] - 2.0 * w[1] + w[2]).abs()).fold(0.0, f32::max)
}

#[test]
fn matroska_audio_plays_without_holes() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let cases: [(&str, u32, &[&str]); 5] = [
        ("tone_aac_48k.mkv", 48_000, &["-c:a", "aac", "-b:a", "160k"]),
        ("tone_aac_44k.mkv", 44_100, &["-c:a", "aac", "-b:a", "160k"]),
        ("tone_mp3_44k.mkv", 44_100, &["-c:a", "libmp3lame", "-b:a", "192k"]),
        ("tone_mp2_44k.mkv", 44_100, &["-c:a", "mp2", "-b:a", "192k"]),
        ("tone_ac3_44k.mkv", 44_100, &["-c:a", "ac3", "-b:a", "192k"]),
    ];
    for (name, rate, codec) in cases {
        let input = format!("sine=frequency=440:sample_rate={rate}:duration=3");
        let args: Vec<&str> = ["-f", "lavfi", "-i", input.as_str(), "-af", "volume=4"].iter().copied().chain(codec.iter().copied()).collect();
        let Some(path) = fixture(&ff, name, &args) else {
            eprintln!("SKIPPED ({name}): this ffmpeg can't encode it");
            continue;
        };
        let src = filmcraft_codecs::open_bytes(name, bytes(&path)).unwrap();
        assert_eq!(src.info().audio().map(|a| a.sample_rate), Some(rate), "{name}");
        let frames = rate as usize * 29 / 10;
        let mut ours = Vec::with_capacity(frames);
        while ours.len() < frames {
            let n = 512.min(frames - ours.len());
            ours.extend_from_slice(&src.audio(ours.len() as i64, n, rate).unwrap().channels[0]);
        }
        let theirs = ffmpeg_audio_f32(&ff, &path, &[]);
        // past the codecs' start-up (priming, MP3's first frame)
        let skip = rate as usize / 10;
        let (r_ours, r_theirs) = (roughness(&ours, skip), roughness(&theirs[..theirs.len().min(frames)], skip));
        let peak = ours.iter().skip(skip).fold(0f32, |m, x| m.max(x.abs()));
        println!("{name}: peak {peak:.3}, max |second difference| ours {r_ours:.5}, ffmpeg {r_theirs:.5}");
        assert!(peak > 0.4, "{name}: the tone is missing (peak {peak})");
        assert!(r_ours < 2.0 * r_theirs + 0.005, "{name}: clicks between packets (|Δ²| {r_ours} vs ffmpeg's {r_theirs})");
    }
}
