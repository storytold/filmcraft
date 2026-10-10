//! Whisper end-to-end on real speech, when the weights are on this machine.
//!
//! Never downloads. Looks for the model (`$FILMCRAFT_SPEECH_MODEL`, default `whisper-tiny`) in
//! `$FILMCRAFT_MODELS_DIR/<id>/`, then `<repo>/target/models/<id>/` (put a model there by hand, or
//! let the app download it into the data directory and point `FILMCRAFT_MODELS_DIR` at
//! `<data dir>/models`). Speech samples are `<dir>/*/<name>.f32` (mono 16 kHz little-endian f32)
//! with the reference text in `<name>.txt`, where `<dir>` is `$FILMCRAFT_SPEECH_FIXTURES` or
//! `<repo>/target/fixtures/speech`; without either the test prints SKIPPED.
#![cfg(feature = "whisper")]

use std::path::{Path, PathBuf};

use filmcraft_speech::{Options, Transcriber, models, word_error_rate};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn model_dir(id: &str) -> Option<PathBuf> {
    let m = models::find(id)?;
    let mut roots: Vec<PathBuf> = std::env::var_os("FILMCRAFT_MODELS_DIR").map(PathBuf::from).into_iter().collect();
    roots.push(repo().join("target/models"));
    // worktrees: the main checkout's target
    if let Ok(main) = std::fs::canonicalize(repo())
        && let Some(p) = main.ancestors().find(|p| p.join(".git").is_dir())
    {
        roots.push(p.join("target/models"));
    }
    roots.into_iter().find(|r| models::installed(r, m)).map(|r| models::model_dir(&r, m))
}

fn samples() -> Vec<(PathBuf, String)> {
    let mut roots: Vec<PathBuf> = std::env::var_os("FILMCRAFT_SPEECH_FIXTURES").map(PathBuf::from).into_iter().collect();
    roots.push(repo().join("target/fixtures/speech"));
    if let Ok(main) = std::fs::canonicalize(repo())
        && let Some(p) = main.ancestors().find(|p| p.join(".git").is_dir())
    {
        roots.push(p.join("target/fixtures/speech"));
    }
    let mut out = Vec::new();
    for r in roots {
        let Ok(dirs) = std::fs::read_dir(&r) else { continue };
        for d in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(d.path()) else { continue };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().is_some_and(|e| e == "f32")
                    && let Ok(txt) = std::fs::read_to_string(p.with_extension("txt"))
                {
                    out.push((p, txt.trim().to_string()));
                }
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out.sort();
    out
}

fn read_f32(p: &Path) -> Vec<f32> {
    std::fs::read(p).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

#[test]
fn model_transcribes_public_domain_speech() {
    let id = std::env::var("FILMCRAFT_SPEECH_MODEL").unwrap_or_else(|_| "whisper-tiny".into());
    let Some(dir) = model_dir(&id) else {
        eprintln!("SKIPPED: {id} weights not found (see the test's module docs)");
        return;
    };
    let samples = samples();
    if samples.is_empty() {
        eprintln!("SKIPPED: no speech samples in target/fixtures/speech");
        return;
    }
    let w = filmcraft_speech::whisper::Whisper::load(&dir, &id).unwrap();
    let opts = Options { language: Some("en".into()), diarize: false, ..Default::default() };
    let (mut errs, mut words) = (0.0, 0usize);
    for (p, reference) in samples.iter().take(8) {
        let audio = read_f32(p);
        let t = w.transcribe(&audio, &opts, &mut |_, _| true).unwrap();
        let hyp = t.text();
        let n = reference.split_whitespace().count();
        errs += word_error_rate(reference, &hyp) * n as f64;
        words += n;
        // word times: in order, inside the audio
        let end = filmcraft_speech::sample_tick(audio.len() as i64);
        t.check().unwrap();
        assert!(t.words.iter().all(|x| x.end <= end), "{}", p.display());
    }
    let wer = errs / words.max(1) as f64;
    eprintln!("{id} WER {:.1}% over {words} words", wer * 100.0);
    assert!(wer < if id == "whisper-tiny" { 0.25 } else { 0.15 }, "WER {wer}");
}
