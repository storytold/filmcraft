//! Fidelity harness (docs/gaps.md G1): does FilmCraft render what Premiere Pro renders?
//!
//! One run (`cargo xtask fidelity`, or the `fidelity` binary of this crate):
//!
//! 1. generates the corpus media with our own generators and encoders ([`corpus::ensure_media`]:
//!    ProRes 422 HQ QuickTime and 24-bit WAV, deterministic, into `<out>/media/`);
//! 2. builds every authored case ([`corpus::CASES`]), writes it as FCP7 XML, OpenTimelineIO and
//!    a FilmCraft project (`<out>/cases/<id>/`), and renders it through the export pipeline to
//!    numbered PNG stills or a float WAV (`<out>/ours/<id>/`);
//! 3. writes `manifest.json` and `PREMIERE.md`, the protocol for rendering the same cases in
//!    Premiere into `plan/premiere/fidelity/<id>/` (maintainer-local, gitignored);
//! 4. reads both renders back through the same decoders ([`reference`]), scores them
//!    ([`score`]: PSNR and CIEDE2000 for pictures, aligned sample-level SNR for sound) and writes
//!    `fidelity-report.md` and `fidelity-results.json` ([`report`]).
//!
//! Without reference renders the harness still does 1–3 and reports every case as *no
//! reference*, so it builds, tests and runs on any checkout; the Premiere side is a documented
//! manual step, never something this code drives.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod corpus;
pub mod manifest;
pub mod reference;
pub mod report;
pub mod score;

use std::path::{Path, PathBuf};

use filmcraft_export::{AudioSettings, ExportSettings, Format, Progress};
use serde::{Deserialize, Serialize};

pub use corpus::{CASES, CaseDef, Compare, CorpusSettings, PremiereEffect, PremiereValue, Tolerance};
use manifest::{CaseEntry, Manifest};
use reference::Found;
use score::{AudioScore, ScoreError, VideoScore};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("media {0}: {1}")]
    Media(PathBuf, String),
    #[error("case {0}: {1}")]
    Case(String, String),
    #[error("export {0}: {1}")]
    Export(PathBuf, String),
    #[error("interchange ({0}): {1}")]
    Interchange(String, String),
    #[error("reference {0}: {1}")]
    Reference(PathBuf, String),
    #[error(transparent)]
    Score(#[from] ScoreError),
    #[error("{0}")]
    Other(String),
}

/// What a run does and where.
#[derive(Clone, Debug)]
pub struct Options {
    /// Everything the harness writes goes here (`target/fidelity` by default).
    pub out_dir: PathBuf,
    /// Where Premiere's renders are (`plan/premiere/fidelity`); `None` measures nothing.
    pub reference_dir: Option<PathBuf>,
    /// Case ids to run (all when empty).
    pub only: Vec<String>,
    pub settings: CorpusSettings,
}

/// How a case did against its tolerance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "verdict", content = "why")]
pub enum Verdict {
    Pass,
    Fail(Vec<String>),
    /// Failed as expected: the case carries a `known_failure` reason (left out of the pass rate).
    Known(Vec<String>),
    NoReference,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseResult {
    pub id: String,
    pub title: String,
    pub group: String,
    pub effect: String,
    pub compare: Compare,
    pub xml_complete: bool,
    pub tolerance: Tolerance,
    pub ours_dir: PathBuf,
    pub reference_dir: PathBuf,
    pub reference: Option<Found>,
    pub video: Option<VideoScore>,
    pub audio: Option<AudioScore>,
    pub verdict: Verdict,
    /// Why the case is expected to fail for now, when it is (`CaseDef::known_failure`).
    pub known_failure: Option<String>,
    /// Interchange exporter warnings for this case.
    pub warnings: Vec<String>,
}

/// What a run produced.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub out_dir: PathBuf,
    pub reference_dir: Option<PathBuf>,
    pub results: Vec<CaseResult>,
    pub report: PathBuf,
    pub manifest: PathBuf,
    pub premiere_md: PathBuf,
    pub results_json: PathBuf,
}

impl Outcome {
    /// Cases with a reference render (passed or failed).
    pub fn compared(&self) -> usize {
        self.results.iter().filter(|r| !matches!(r.verdict, Verdict::NoReference)).count()
    }

    /// Compared cases that are marked as known failures (whatever they scored).
    pub fn known(&self) -> usize {
        self.results.iter().filter(|r| r.known_failure.is_some() && !matches!(r.verdict, Verdict::NoReference)).count()
    }

    /// Passing cases among the ones expected to pass.
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.verdict == Verdict::Pass && r.known_failure.is_none()).count()
    }

    /// Cases marked as known failures that pass now: the mark can come off.
    pub fn fixed(&self) -> usize {
        self.results.iter().filter(|r| r.verdict == Verdict::Pass && r.known_failure.is_some()).count()
    }

    /// Passing cases in percent of the compared ones expected to pass (0 when there are none).
    pub fn pass_rate(&self) -> f64 {
        let c = self.compared().saturating_sub(self.known());
        if c == 0 { 0.0 } else { self.passed() as f64 * 100.0 / c as f64 }
    }

    pub fn summary(&self) -> String {
        report::summary(self)
    }
}

/// Judge the scores of a case against its tolerance.
pub fn judge(def: &CaseDef, video: Option<&VideoScore>, audio: Option<&AudioScore>) -> Verdict {
    let t = def.tolerance;
    let mut why = Vec::new();
    match def.compare {
        Compare::Video => {
            let Some(v) = video else { return Verdict::Fail(vec!["the reference render has no picture".into()]) };
            if let (Some(min), Some(p)) = (t.min_psnr_db, v.psnr_db)
                && p < min
            {
                why.push(format!("PSNR {p:.2} dB < {min} dB"));
            }
            if let Some(max) = t.max_de2000_mean
                && v.de2000_mean > max
            {
                why.push(format!("mean ΔE₀₀ {:.3} > {max}", v.de2000_mean));
            }
            if let Some(max) = t.max_de2000_p95
                && v.de2000_p95 > max
            {
                why.push(format!("p95 ΔE₀₀ {:.2} > {max}", v.de2000_p95));
            }
            if v.frames_reference != v.frames_ours {
                why.push(format!("{} frames in the reference, {} in ours", v.frames_reference, v.frames_ours));
            }
        }
        Compare::Audio => {
            let Some(a) = audio else { return Verdict::Fail(vec!["the reference render has no sound".into()]) };
            if let (Some(min), Some(s)) = (t.min_snr_db, a.snr_db)
                && s < min
            {
                why.push(format!("SNR {s:.2} dB < {min} dB"));
            }
            if let Some(max) = t.max_lag_samples
                && a.lag.abs() > max
            {
                why.push(format!("lag {} samples beyond ±{max}", a.lag));
            }
        }
    }
    if why.is_empty() { Verdict::Pass } else { Verdict::Fail(why) }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    }
    std::fs::write(path, bytes).map_err(|e| Error::Io(path.to_path_buf(), e))
}

/// Write the case as FCP7 XML, OTIO and `.fcproj`; returns the exporters' warnings.
fn write_interchange(built: &corpus::BuiltCase, dir: &Path) -> Result<Vec<String>, Error> {
    let id = built.def.id;
    let mut warnings = Vec::new();
    let opts = filmcraft_interchange::ExportOptions { name: Some(id.to_string()), ..Default::default() };
    for (format, ext) in [(filmcraft_interchange::Format::Fcp7Xml, "xml"), (filmcraft_interchange::Format::Otio, "otio")] {
        let (bytes, report) =
            filmcraft_interchange::export(&built.project, built.seq, format, &opts).map_err(|e| Error::Interchange(format!("{id} {ext}"), e.to_string()))?;
        write(&dir.join(format!("{id}.{ext}")), &bytes)?;
        for e in report.entries {
            if e.level == filmcraft_interchange::Level::Warning {
                warnings.push(format!("{ext}: {}", e.message));
            }
        }
    }
    write(&dir.join(format!("{id}.fcproj")), &filmcraft_format::encode(&built.project, true))?;
    Ok(warnings)
}

/// Render the case through the export pipeline into `dir` (emptied first).
fn render_ours(built: &corpus::BuiltCase, dir: &Path) -> Result<(), Error> {
    if dir.is_dir() {
        std::fs::remove_dir_all(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    let id = built.def.id;
    let settings = match built.def.compare {
        Compare::Video => ExportSettings {
            format: Format::PngSequence,
            path: dir.join(format!("{id}.png")).to_string_lossy().to_string(),
            range: Some(built.range),
            include_audio: false,
            ..Default::default()
        },
        Compare::Audio => ExportSettings {
            format: Format::Wav,
            path: dir.join(format!("{id}.wav")).to_string_lossy().to_string(),
            range: Some(built.range),
            include_audio: true,
            audio: AudioSettings { bits: 32, float: true, ..Default::default() },
            ..Default::default()
        },
    };
    filmcraft_export::export(&built.project, built.seq, &settings, &built.sources, &Progress::default())
        .map_err(|e| Error::Export(dir.to_path_buf(), e.to_string()))?;
    Ok(())
}

/// Score one case's render against a reference directory's contents.
fn score_case(def: &CaseDef, ours: &Found, reference: &Found, s: &CorpusSettings) -> Result<(Option<VideoScore>, Option<AudioScore>), Error> {
    let max_frames = usize::try_from(s.frames()).unwrap_or(0).saturating_add(2);
    let max_samples = usize::try_from(s.samples()).unwrap_or(0).saturating_add(s.sample_rate as usize);
    match def.compare {
        Compare::Video => {
            let o = reference::read_video(ours, max_frames)?;
            let r = reference::read_video(reference, max_frames)?;
            if r.is_empty() {
                return Ok((None, None));
            }
            Ok((Some(score::video_score(&o, &r)?), None))
        }
        Compare::Audio => {
            let o = reference::read_audio(ours, s.sample_rate, max_samples)?;
            let r = reference::read_audio(reference, s.sample_rate, max_samples)?;
            if r.is_empty() {
                return Ok((None, None));
            }
            // alignment search: up to one frame either way
            let max_lag = usize::try_from(i64::from(s.sample_rate) * s.frame_rate.den.max(1) / s.frame_rate.num.max(1)).unwrap_or(2000);
            Ok((None, Some(score::audio_score(&o, &r, max_lag)?)))
        }
    }
}

/// Run the harness.
pub fn run(opts: &Options) -> Result<Outcome, Error> {
    let out = &opts.out_dir;
    std::fs::create_dir_all(out).map_err(|e| Error::Io(out.clone(), e))?;
    let defs: Vec<&'static CaseDef> = if opts.only.is_empty() {
        CASES.iter().collect()
    } else {
        let mut v = Vec::new();
        for id in &opts.only {
            v.push(
                corpus::case(id)
                    .ok_or_else(|| Error::Other(format!("no case `{id}` (known: {})", CASES.iter().map(|c| c.id).collect::<Vec<_>>().join(", "))))?,
            );
        }
        v
    };
    let media = corpus::ensure_media(&out.join("media"), &opts.settings)?;
    let reference_root = opts.reference_dir.clone().unwrap_or_else(|| PathBuf::from("plan/premiere/fidelity"));
    let mut results = Vec::with_capacity(defs.len());
    let mut entries = Vec::with_capacity(defs.len());
    for def in defs {
        let built = corpus::build(def, &media, &opts.settings)?;
        let case_dir = out.join("cases").join(def.id);
        let warnings = write_interchange(&built, &case_dir)?;
        let ours_dir = out.join("ours").join(def.id);
        render_ours(&built, &ours_dir)?;
        let ours = reference::locate(&ours_dir)?.ok_or_else(|| Error::Export(ours_dir.clone(), "the export wrote nothing".into()))?;
        let reference_dir = reference_root.join(def.id);
        let reference = match &opts.reference_dir {
            Some(_) => reference::locate(&reference_dir)?,
            None => None,
        };
        let (video, audio, verdict) = match &reference {
            Some(r) => {
                let (v, a) = score_case(def, &ours, r, &opts.settings)?;
                let verdict = match (judge(def, v.as_ref(), a.as_ref()), def.known_failure) {
                    (Verdict::Fail(why), Some(_)) => Verdict::Known(why),
                    (verdict, _) => verdict,
                };
                (v, a, verdict)
            }
            None => (None, None, Verdict::NoReference),
        };
        entries.push(CaseEntry {
            id: def.id.into(),
            title: def.title.into(),
            group: def.group.into(),
            effect: def.effect.into(),
            compare: def.compare,
            about: def.about.into(),
            xml_complete: def.xml_complete,
            premiere_steps: def.premiere_steps.iter().map(|s| s.to_string()).collect(),
            premiere_effects: def.premiere_effects.iter().map(Into::into).collect(),
            known_failure: def.known_failure.map(str::to_string),
            fcp7_xml: case_dir.join(format!("{}.xml", def.id)).to_string_lossy().to_string(),
            otio: case_dir.join(format!("{}.otio", def.id)).to_string_lossy().to_string(),
            fcproj: case_dir.join(format!("{}.fcproj", def.id)).to_string_lossy().to_string(),
            warnings: warnings.clone(),
            ours_dir: ours_dir.to_string_lossy().to_string(),
            reference_dir: reference_dir.to_string_lossy().to_string(),
            frames: opts.settings.frames(),
            samples: opts.settings.samples(),
            tolerance: def.tolerance,
        });
        results.push(CaseResult {
            id: def.id.into(),
            title: def.title.into(),
            group: def.group.into(),
            effect: def.effect.into(),
            compare: def.compare,
            xml_complete: def.xml_complete,
            tolerance: def.tolerance,
            ours_dir,
            reference_dir,
            reference,
            video,
            audio,
            verdict,
            known_failure: def.known_failure.map(str::to_string),
            warnings,
        });
    }
    let manifest = Manifest {
        version: manifest::VERSION,
        generated_by: format!("filmcraft-fidelity {}", env!("CARGO_PKG_VERSION")),
        settings: opts.settings,
        media: media.files().iter().map(|p| manifest::media_entry(p)).collect::<Result<Vec<_>, _>>()?,
        cases: entries,
        reference_root: reference_root.to_string_lossy().to_string(),
    };
    let manifest_path = out.join("manifest.json");
    write(&manifest_path, serde_json::to_string_pretty(&manifest).map_err(|e| Error::Other(e.to_string()))?.as_bytes())?;
    let premiere_md = out.join("PREMIERE.md");
    write(&premiere_md, manifest::premiere_md(&manifest).as_bytes())?;
    let mut outcome = Outcome {
        out_dir: out.clone(),
        reference_dir: opts.reference_dir.clone(),
        results,
        report: out.join("fidelity-report.md"),
        manifest: manifest_path,
        premiere_md,
        results_json: out.join("fidelity-results.json"),
    };
    write(&outcome.report, report::markdown(&outcome).as_bytes())?;
    write(&outcome.results_json, serde_json::to_string_pretty(&outcome.results).map_err(|e| Error::Other(e.to_string()))?.as_bytes())?;
    outcome.results.shrink_to_fit();
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_export::SampleFormat;
    use score::Frame;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("filmcraft-fidelity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let p = e.unwrap().path();
            std::fs::copy(&p, to.join(p.file_name().unwrap())).unwrap();
        }
    }

    fn video_def() -> &'static CaseDef {
        corpus::case("cut").unwrap()
    }

    fn audio_def() -> &'static CaseDef {
        corpus::case("audio_gain_minus6").unwrap()
    }

    fn vscore(psnr: Option<f64>, de_mean: f64, de_p95: f64, frames: (usize, usize)) -> VideoScore {
        VideoScore {
            frames_ours: frames.0,
            frames_reference: frames.1,
            frames_compared: frames.0.min(frames.1),
            psnr_db: psnr,
            worst_frame: psnr.map(|p| (0, p)),
            mean_abs: 0.0,
            p99_abs: 0,
            max_abs: 0,
            de2000_mean: de_mean,
            de2000_p95: de_p95,
            de2000_max: de_p95,
            per_frame: vec![],
        }
    }

    fn ascore(snr: Option<f64>, lag: i64) -> AudioScore {
        AudioScore {
            channels_ours: 2,
            channels_reference: 2,
            frames_ours: 100,
            frames_reference: 100,
            frames_compared: 100,
            length_delta: 0,
            lag,
            max_lag: 10,
            snr_db: snr,
            per_channel_snr_db: vec![snr, snr],
            max_abs: 0.0,
            rms_error: 0.0,
            peak_ours: 1.0,
            peak_reference: 1.0,
        }
    }

    #[test]
    fn verdicts_follow_the_tolerance() {
        let v = video_def();
        assert_eq!(judge(v, Some(&vscore(None, 0.0, 0.0, (24, 24))), None), Verdict::Pass);
        assert_eq!(judge(v, Some(&vscore(Some(45.0), 0.5, 2.0, (24, 24))), None), Verdict::Pass);
        let Verdict::Fail(why) = judge(v, Some(&vscore(Some(30.0), 3.0, 1.0, (24, 25))), None) else { panic!("should fail") };
        assert_eq!(why.len(), 3, "{why:?}");
        assert!(
            why[0].contains("PSNR 30.00 dB < 40") && why[1].contains("mean ΔE₀₀ 3.000 > 1") && why[2].contains("25 frames in the reference, 24 in ours"),
            "{why:?}"
        );
        assert!(matches!(judge(v, None, Some(&ascore(None, 0))), Verdict::Fail(w) if w[0].contains("no picture")));
        let a = audio_def();
        assert_eq!(judge(a, None, Some(&ascore(None, 0))), Verdict::Pass);
        assert_eq!(judge(a, None, Some(&ascore(Some(60.0), 2))), Verdict::Pass);
        assert!(
            matches!(judge(a, None, Some(&ascore(Some(20.0), 3))), Verdict::Fail(w) if w.len() == 2 && w[0].contains("SNR 20.00 dB < 40") && w[1].contains("lag 3"))
        );
        assert!(matches!(judge(a, Some(&vscore(None, 0.0, 0.0, (1, 1))), None), Verdict::Fail(w) if w[0].contains("no sound")));
    }

    /// The whole pipeline on two cases at a tiny size: no reference, then our own render as a
    /// perfect reference, then a perturbed one.
    #[test]
    fn harness_runs_and_scores_against_a_reference_directory() {
        let out = tmp("run");
        let settings = CorpusSettings { width: 160, height: 90, seconds: 1, ..Default::default() };
        let opts = Options { out_dir: out.clone(), reference_dir: None, only: vec!["cut".into(), "audio_gain_minus6".into()], settings };
        let o = run(&opts).unwrap();
        assert_eq!(o.results.len(), 2);
        assert!(o.results.iter().all(|r| r.verdict == Verdict::NoReference && r.video.is_none() && r.audio.is_none()));
        assert_eq!((o.compared(), o.passed(), o.pass_rate()), (0, 0, 0.0));
        assert!(o.summary().starts_with("fidelity: 0/0 cases pass (0.0%), 2 without a Premiere render"), "{}", o.summary());
        let report = std::fs::read_to_string(&o.report).unwrap();
        assert!(report.contains("| `cut` | Baseline | – | picture | XML | – |") && report.contains("no reference |"), "{report}");
        assert!(report.contains("- `cut`: export `cut.png (numbered stills)` into"));
        let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(&o.manifest).unwrap()).unwrap();
        assert_eq!(manifest.cases.len(), 2);
        assert_eq!(manifest.media.len(), 4);
        assert!(manifest.cases[0].fcp7_xml.ends_with("cases/cut/cut.xml"));
        let xml = std::fs::read_to_string(&manifest.cases[0].fcp7_xml).unwrap();
        assert!(xml.contains("<xmeml") && xml.contains("footage_a.mov") && xml.contains("footage_b.mov"), "{xml}");
        assert!(std::fs::read_to_string(&manifest.cases[0].otio).unwrap().contains("\"OTIO_SCHEMA\": \"Timeline.1\""));
        assert!(std::fs::read(&manifest.cases[0].fcproj).unwrap().len() > 100);
        let premiere = std::fs::read_to_string(&o.premiere_md).unwrap();
        assert!(premiere.contains("#### `cut`: Hard cut") && premiere.contains("160×90, 24 fps, 48000 Hz"), "{premiere}");
        let stills = std::fs::read_dir(out.join("ours/cut")).unwrap().count();
        assert_eq!(stills, 24, "one PNG per frame");
        assert!(out.join("ours/audio_gain_minus6/audio_gain_minus6.wav").is_file());

        // our own render as the reference: everything identical
        let reference = tmp("ref");
        copy_dir(&out.join("ours/cut"), &reference.join("cut"));
        copy_dir(&out.join("ours/audio_gain_minus6"), &reference.join("audio_gain_minus6"));
        let with_ref = Options { reference_dir: Some(reference.clone()), ..opts.clone() };
        let o = run(&with_ref).unwrap();
        assert!(o.results.iter().all(|r| r.verdict == Verdict::Pass), "{:?}", o.results.iter().map(|r| &r.verdict).collect::<Vec<_>>());
        let v = o.results[0].video.as_ref().unwrap();
        assert_eq!((v.frames_compared, v.psnr_db, v.max_abs, v.de2000_max), (24, None, 0, 0.0));
        let a = o.results[1].audio.as_ref().unwrap();
        assert_eq!((a.frames_compared, a.snr_db, a.lag, a.channels_reference), (48_000, None, 0, 2));
        assert_eq!((o.compared(), o.known(), o.passed(), o.fixed(), o.pass_rate()), (2, 1, 1, 1, 100.0));
        assert!(o.summary().starts_with("fidelity: 1/1 cases pass (100.0%), 1 known failure (1 passing now), 0 without"), "{}", o.summary());
        let report = std::fs::read_to_string(&o.report).unwrap();
        assert!(report.contains("| 24 / 24 frames | ∞ | 0.000 | 0.00 | 0 | – | – | **pass** |"), "{report}");
        assert!(report.contains("| 48000 / 48000 samples | – | – | – | 0.00000 | ∞ | 0 | **pass** (marked known) |"), "{report}");
        assert!(report.contains("## Passing though marked known (1)") && report.contains("- `audio_gain_minus6`: remove its `known_failure`"), "{report}");

        // a perturbed reference: frame 3 redder, the sound at half level
        let frame = reference.join("cut/cut003.png");
        let found = reference::locate(&reference.join("cut")).unwrap().unwrap();
        let mut frames = reference::read_video(&found, 24).unwrap();
        let f: &mut Frame = &mut frames[3];
        for px in f.rgba.chunks_mut(4) {
            px[0] = px[0].saturating_add(40);
        }
        std::fs::write(&frame, filmcraft_export::encode_png(f.rgba.clone(), f.w, f.h).unwrap()).unwrap();
        let wav = reference.join("audio_gain_minus6/audio_gain_minus6.wav");
        let found = reference::locate(&reference.join("audio_gain_minus6")).unwrap().unwrap();
        let audio = reference::read_audio(&found, 48_000, 48_000).unwrap();
        let interleaved: Vec<f32> = (0..audio[0].len()).flat_map(|i| [audio[0][i] * 0.5, audio[1][i] * 0.5]).collect();
        std::fs::write(&wav, filmcraft_export::write_wav(&interleaved, 2, 48_000, SampleFormat::Float32).unwrap()).unwrap();
        let o = run(&with_ref).unwrap();
        let Verdict::Fail(why) = &o.results[0].verdict else { panic!("{:?}", o.results[0].verdict) };
        assert!(why.iter().any(|w| w.starts_with("PSNR")), "{why:?}");
        let v = o.results[0].video.as_ref().unwrap();
        assert_eq!(v.worst_frame.map(|(i, _)| i), Some(3));
        assert_eq!(v.max_abs, 40);
        // the audio case is marked as a known failure, so it fails as *known*
        let Verdict::Known(why) = &o.results[1].verdict else { panic!("{:?}", o.results[1].verdict) };
        // reference = ½ ours: the error is half the signal's amplitude, so SNR is 0 dB
        assert!(why[0].starts_with("SNR 0.00 dB < 40"), "{why:?}");
        assert_eq!(o.results[1].audio.as_ref().unwrap().lag, 0);
        assert_eq!((o.compared(), o.known(), o.passed(), o.fixed(), o.pass_rate()), (2, 1, 0, 0, 0.0));
        assert!(o.summary().starts_with("fidelity: 0/1 cases pass (0.0%), 1 known failure, 0 without"), "{}", o.summary());
        let report = std::fs::read_to_string(&o.report).unwrap();
        assert!(report.contains("## Failing (1)") && report.contains("- worst frame 3: PSNR"), "{report}");
        assert!(report.contains("## Known failures (1)") && report.contains("| **known** |") && report.contains("stereo clip"), "{report}");

        // an unknown case is refused by name
        let bad = Options { only: vec!["nope".into()], ..opts };
        assert!(run(&bad).unwrap_err().to_string().contains("no case `nope`"));
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::remove_dir_all(&reference);
    }
}
