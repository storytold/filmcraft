//! Parakeet TDT end-to-end on real speech, when the weights are on this machine.
//!
//! Never downloads. Looks for the model in `$FILMCRAFT_MODELS_DIR/<id>/`, then
//! `<repo>/target/models/<id>/` (and the main checkout's, from a worktree). Speech samples are
//! `<repo>/target/fixtures/speech/**/<name>.f32` (mono 16 kHz little-endian f32) with the
//! reference text in `<name>.txt`, or the directory in `$FILMCRAFT_SPEECH_SAMPLES`; without
//! weights or samples the tests print SKIPPED.
#![cfg(feature = "parakeet")]

use std::path::{Path, PathBuf};

use filmcraft_speech::parakeet::Parakeet;
use filmcraft_speech::{Options, Transcriber, models, word_error_rate};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main_checkout() -> Option<PathBuf> {
    let r = std::fs::canonicalize(repo()).ok()?;
    r.ancestors().find(|p| p.join(".git").is_dir()).map(Path::to_path_buf)
}

fn model_dir(id: &str) -> Option<PathBuf> {
    let m = models::find(id)?;
    let mut roots: Vec<PathBuf> = std::env::var_os("FILMCRAFT_MODELS_DIR").map(PathBuf::from).into_iter().collect();
    roots.push(repo().join("target/models"));
    roots.extend(main_checkout().map(|p| p.join("target/models")));
    roots.into_iter().find(|r| models::installed(r, m)).map(|r| models::model_dir(&r, m))
}

fn samples() -> Vec<(PathBuf, String)> {
    let mut roots: Vec<PathBuf> = std::env::var_os("FILMCRAFT_SPEECH_SAMPLES").map(PathBuf::from).into_iter().collect();
    roots.push(repo().join("target/fixtures/speech"));
    roots.extend(main_checkout().map(|p| p.join("target/fixtures/speech")));
    let mut out = Vec::new();
    for r in roots {
        let mut dirs = vec![r.clone()];
        if let Ok(rd) = std::fs::read_dir(&r) {
            dirs.extend(rd.flatten().map(|d| d.path()).filter(|p| p.is_dir()));
        }
        for d in dirs {
            let Ok(files) = std::fs::read_dir(&d) else { continue };
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

fn load(id: &str) -> Option<Parakeet> {
    let Some(dir) = model_dir(id) else {
        eprintln!("SKIPPED: {id} weights not found (see the test's module docs)");
        return None;
    };
    Some(Parakeet::load(&dir, id).unwrap())
}

#[test]
fn v3_transcribes_public_domain_speech() {
    let samples = samples();
    if samples.is_empty() {
        eprintln!("SKIPPED: no speech samples in target/fixtures/speech");
        return;
    }
    let Some(p) = load("parakeet-tdt-0.6b-v3") else { return };
    let opts = Options { language: None, diarize: false, ..Default::default() };
    let (mut errs, mut words) = (0.0, 0usize);
    for (path, reference) in samples.iter().take(12) {
        let audio = read_f32(path);
        let t = p.transcribe(&audio, &opts, &mut |_, _| true).unwrap();
        let n = reference.split_whitespace().count();
        errs += word_error_rate(reference, &t.text()) * n as f64;
        words += n;
        let end = filmcraft_speech::sample_tick(audio.len() as i64);
        t.check().unwrap();
        assert!(t.words.iter().all(|w| w.end <= end && w.start < w.end && (0.0..=1.0).contains(&w.confidence)), "{}", path.display());
    }
    let wer = errs / words.max(1) as f64;
    eprintln!("parakeet-tdt-0.6b-v3 WER {:.1}% over {words} words", wer * 100.0);
    assert!(wer < 0.10, "WER {wer}");
}

#[test]
fn checkpoint_front_end_tables_match_the_computed_ones() {
    for id in ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v2"] {
        let Some(dir) = model_dir(id) else {
            eprintln!("SKIPPED: {id} weights not found");
            continue;
        };
        let m = models::find(id).unwrap();
        let nemo = filmcraft_speech::nemo::Nemo::open(&dir.join(m.files[0].name)).unwrap();
        let mut f = nemo.file().unwrap();
        let (shape, fb) = nemo.read_f32(&mut f, "preprocessor.featurizer.fb").unwrap();
        assert_eq!(shape, vec![1, 128, 257]);
        let ours = filmcraft_speech::parakeet::features::mel_filters(16_000.0, 512, 128, 0.0, 8_000.0);
        let d = fb.iter().zip(&ours).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(d < 1e-6, "{id}: filterbank differs by {d}");
        let (_, win) = nemo.read_f32(&mut f, "preprocessor.featurizer.window").unwrap();
        let d = win.iter().zip(&filmcraft_speech::parakeet::features::hann(400)).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(d < 1e-6, "{id}: window differs by {d}");
        assert_eq!(nemo.tensors.len(), 725, "{id}");
    }
}

#[test]
fn v3_long_audio_joins_pieces_without_losing_words() {
    let samples = samples();
    if samples.len() < 2 {
        eprintln!("SKIPPED: need speech samples in target/fixtures/speech");
        return;
    }
    let Some(p) = load("parakeet-tdt-0.6b-v3") else { return };
    // ~100 s of speech made of repeated samples with short gaps: long enough to be cut into pieces
    let mut audio = Vec::new();
    let mut reference = String::new();
    let mut i = 0;
    while audio.len() < 16_000 * 100 {
        let (path, text) = &samples[i % samples.len()];
        audio.extend(std::iter::repeat_n(0.0f32, 8_000));
        audio.extend(read_f32(path));
        reference.push_str(text);
        reference.push(' ');
        i += 1;
    }
    assert!(filmcraft_speech::parakeet::pieces(&audio).len() > 1);
    let opts = Options { language: Some("en".into()), diarize: false, ..Default::default() };
    let mut last = 0.0;
    let t = p
        .transcribe(&audio, &opts, &mut |f, _| {
            assert!(f >= last);
            last = f;
            true
        })
        .unwrap();
    t.check().unwrap();
    let wer = word_error_rate(&reference, &t.text());
    eprintln!("long-form WER {:.1}% over {} words", wer * 100.0, reference.split_whitespace().count());
    assert!(wer < 0.10, "WER {wer}");
    // cancelling stops the run
    assert_eq!(p.transcribe(&audio, &opts, &mut |f, _| f < 0.3).err(), Some(filmcraft_speech::SpeechError::Cancelled));
}

#[test]
fn v2_is_english_only() {
    let samples = samples();
    let Some((path, reference)) = samples.first() else {
        eprintln!("SKIPPED: no speech samples in target/fixtures/speech");
        return;
    };
    let Some(p) = load("parakeet-tdt-0.6b-v2") else { return };
    assert!(!p.multilingual());
    let de = Options { language: Some("de".into()), diarize: false, ..Default::default() };
    assert!(p.transcribe(&[0.0; 16_000], &de, &mut |_, _| true).is_err());
    let t = p.transcribe(&read_f32(path), &Options { diarize: false, ..Default::default() }, &mut |_, _| true).unwrap();
    assert_eq!(t.language, "en");
    assert!(word_error_rate(reference, &t.text()) < 0.2, "{}", t.text());
}
