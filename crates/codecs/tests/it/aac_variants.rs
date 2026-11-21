//! Real-world `.aac` file shapes (#1365): the reporter's files came from the wild, so probe the
//! shapes ffmpeg produces, not just our own encoder's output.

use std::process::Command;
use std::sync::Arc;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn make(args: &[&str], out: &str) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("fc_aac_variants");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(out);
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y").arg("-loglevel").arg("error");
    for a in args {
        cmd.arg(a);
    }
    cmd.arg(&path);
    if !cmd.status().ok()?.success() {
        return None;
    }
    std::fs::read(&path).ok()
}

fn try_import(name: &str, bytes: Vec<u8>) -> String {
    match filmcraft_codecs::open_bytes(name, Arc::from(bytes)) {
        Ok(src) => {
            let dur = src.info().duration.seconds();
            match src.audio(0, 4800, 48000) {
                Ok(b) => format!("ok: {dur:.3} s, {} ch", b.channels.len()),
                Err(e) => format!("audio ERR: {e}"),
            }
        }
        Err(e) => format!("open ERR: {e}"),
    }
}

#[test]
fn wild_aac_shapes_import() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg; skipping");
        return;
    }
    let mut results = Vec::new();
    if let Some(b) =
        make(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=1", "-c:a", "aac", "-b:a", "96k", "-f", "adts"], "plain_44k.aac")
    {
        results.push(format!("plain ADTS 44.1k: {}", try_import("plain.aac", b)));
    } else {
        results.push("plain ADTS 44.1k: GEN FAILED".into());
    }
    if let Some(b) =
        make(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1", "-c:a", "aac", "-f", "adts", "-write_id3v1", "1"], "id3tagged.aac")
    {
        results.push(format!("ID3-tagged ADTS:  {}", try_import("tagged.aac", b)));
    } else {
        results.push("ID3-tagged ADTS: GEN FAILED".into());
    }
    // MP4 container with a .aac extension (very common in the wild: downloads mislabeled)
    if let Some(b) = make(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1", "-c:a", "aac"], "variants_m4a.m4a") {
        results.push(format!("m4a-as-.aac:       {}", try_import("song.aac", b)));
    } else {
        results.push("m4a-as-.aac: GEN FAILED".into());
    }
    println!("\n{}", results.join("\n"));
    // everything above must import
    for r in &results {
        assert!(r.contains("ok:"), "{r}");
    }
}
