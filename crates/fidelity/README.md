# filmcraft-fidelity

The fidelity harness ([docs/gaps.md G1](../../docs/gaps.md#g1-nothing-compares-our-output-with-premieres)):
does FilmCraft render what Adobe Premiere Pro renders? Layer L3 (depends on `render`, `export`,
`interchange`, `format`, `codecs`, `media`, `project`, `color`, `time`; no UI; builds for wasm32).
One command, `cargo xtask fidelity`, does everything that can be done on this side. The Premiere
side is a documented step (`PREMIERE.md`, generated per run) that a person follows, or that
`premiere/render.py` performs on a machine with Premiere Pro through the small bridge panel in
`premiere/bridge/`; the crate itself never drives Premiere.

```sh
cargo xtask fidelity                 # corpus, exports, our renders, report; scores plan/premiere/fidelity/ if present
cargo xtask fidelity --list          # the cases
cargo xtask fidelity --case cut --case gaussian_blur
cargo xtask fidelity --min 90        # fail when fewer than 90 % of the compared cases pass
cargo xtask fidelity --reference DIR # Premiere renders elsewhere than plan/premiere/fidelity/
python3 crates/fidelity/premiere/render.py target/fidelity/manifest.json   # the Premiere side, by script
```

## What one run does

| Step | Output (under `<target>/fidelity/`) |
|---|---|
| Generate the media with our own generators and encoders: two demo footages and Bars and Tone as ProRes 422 HQ QuickTime with 24-bit PCM, a −12 dBFS 440 Hz tone as WAV; 640×360, 24 fps, 48 kHz, 7 s; deterministic, kept while `media/settings.json` records the same size, rate and length, made again otherwise | `media/` |
| Build every case (`corpus::CASES`, 20 today) as a 3 s sequence on that media and write it as FCP7 XML, OpenTimelineIO and a `.fcproj` | `cases/<id>/<id>.{xml,otio,fcproj}` |
| Render each case through the export pipeline (the same path a user's export takes): numbered PNG stills for picture cases, a 32-bit float WAV for sound cases | `ours/<id>/` |
| Write the manifest and the Premiere protocol: per case, what to import, what to do by hand, where to export | `manifest.json`, `PREMIERE.md` |
| Read both renders back through the same decoders, score them and report | `fidelity-report.md`, `fidelity-results.json` |

Without Premiere renders every case reports *no reference* and the run still succeeds (`--min`
then fails, as `cargo xtask parity --min` does without a reference list). Premiere's output lives in
`plan/premiere/<...>` only: gitignored, never committed, never embedded ([AGENTS.md §1](../../AGENTS.md)).

## The corpus

Every case isolates one thing. *Via* says whether FCP7 XML carries it all (import, export, done)
or the person applies the effect in Premiere after the import (FCP7 XML has no vocabulary for
Premiere's standard effects; the generated `PREMIERE.md` lists the exact steps and values).

| Case | Group | Compares | Via | Isolates |
|---|---|---|---|---|
| `cut` | Baseline | picture | XML | the floor set by ProRes decoding and the colour pipeline alone |
| `motion_scale_position`, `motion_rotation`, `scale_ramp` | Motion | picture | XML | Basic Motion scale / position / rotation, and a linear scale keyframe ramp |
| `opacity_50`, `opacity_ramp` | Opacity | picture | XML | half opacity over another clip; a linear 0 → 100 % fade |
| `cross_dissolve`, `dip_to_black` | Transitions | picture | XML | transition alignment, handles and curves |
| `gaussian_blur`, `brightness_contrast`, `crop`, `black_white`, `invert` | Effects | picture | manual | one standard effect each, at stated values (Gaussian Blur (Legacy): Premiere Pro 2026 gave the plain name to its new Impact-based blur) |
| `speed_50`, `speed_200` | Speed | picture | XML | which source frame each output frame shows (Time Remap) |
| `audio_cut`, `audio_gain_minus6`, `audio_pan_left`, `audio_constant_power`, `audio_level_ramp` | Audio | sound | XML | the sound baseline, the dB law, the balance law, the crossfade curve, dB-vs-gain ramps |

Adding a case is one `CaseDef` in `src/corpus.rs`: a builder function placing clips on the
two video and two audio tracks (`Builder::{video, audio, set, keyframe, add_effect, transition,
speed}`), the Premiere steps when the XML cannot carry it (as text for a person and as
`PremiereEffect`s for the script: Effects panel name plus Effect Controls values), and a tolerance.

## The Premiere side, by script

Premiere Pro has no command line, so the script talks to a panel inside it. One-time setup on a
machine with Premiere Pro (2020 or later) and Adobe Media Encoder:

1. `crates/fidelity/premiere/install.sh` (macOS; `install.ps1` on Windows, untested so far) copies
   `premiere/bridge/` into Premiere's per-user extension folder and turns on CEP's PlayerDebugMode,
   which lets Premiere load an unsigned panel. Restart Premiere.
2. Open any project and open Window ▸ Extensions ▸ **FilmCraft Fidelity** once. The panel stays in
   the workspace; it watches `~/.filmcraft/fidelity-bridge/`, runs the ExtendScript the script
   drops there, and answers in the same directory. Nothing of Adobe's is involved but Premiere's
   own scripting API.
3. Save two export presets from File ▸ Export ▸ Media (Save Preset on the Preset row):
   `fidelity-png` (Format PNG, Export As Sequence, 8 bpc, no alpha, Match Source) and
   `fidelity-wav` (Waveform Audio, 48 kHz, 24-bit, stereo). The script finds them by name under
   `~/Documents/Adobe/Adobe Media Encoder/<version>/Presets/`.

Then `premiere/render.py MANIFEST [--case ID]… [--reference DIR] [--dry-run]` reads
`manifest.json`, makes a fresh `.prproj` under `plan/premiere/fidelity/_premiere/`, and per
case: imports the FCP7 XML (UI suppressed), checks the sequence settings against the corpus,
applies the case's `PremiereEffect`s to the clip on V1 (setting an existing same-named component
when the import made one, as it does for Crop; every value is read back), waits until frame 0
renders the same twice (media linked), and exports through Media Encoder: a PNG sequence with
`fidelity-png`, a WAV with `fidelity-wav`. Without the PNG preset, or with `--frame-export`,
picture cases come out of Premiere's frame export at the exact tick of every frame in one
ExtendScript call; without the WAV preset the sound cases are skipped, their old renders removed, and the run says
so and exits non-zero. Every case's previous render is removed before its import, so a case that
fails leaves no stale render for the harness to score. The
project is saved and closed at the end (`--keep-open` to look at it); a later run with no panel
heartbeat opens the newest saved project in Premiere itself to wake it. Then `cargo xtask
fidelity` scores. Stdlib Python, never run in CI, writes only under the reference root and the
bridge directory. Do not use Premiere while a run drives it.

## Scores

| Side | Metric | Definition |
|---|---|---|
| picture | PSNR | over R, G, B of 8-bit sRGB frames flattened over black, pooled over every compared frame; ∞ when identical; the worst single frame is named |
| picture | ΔE₀₀ | CIEDE2000 (Sharma, Wu & Dalal 2005) per pixel from sRGB → XYZ (D65) → L\*a\*b\*; mean, 95th percentile and maximum. Checked against five of the published test pairs to 10⁻⁴ |
| picture | max diff | the largest per-pixel channel difference, with its mean and 99th percentile |
| sound | lag | the alignment at which the reference best matches ours (least squared error with silence beyond either end, i.e. maximum cross-correlation, by FFT over every lag within ±1 frame); a late or early render is a lag, not noise |
| sound | SNR | reference power over error power after alignment, in dB, overall and per channel; ∞ when identical; also the largest sample error and the RMS error |

Tolerances (`corpus::Tolerance`) are provisional until the first real renders calibrate them:
picture PSNR ≥ 35 dB, mean ΔE₀₀ ≤ 2, p95 ≤ 6 (the baseline cut ≥ 40 dB, ≤ 1, ≤ 3); sound SNR
≥ 40 dB at |lag| ≤ 2 samples. A different frame count is reported as a failure reason too.

A case whose failure is understood and filed carries `known_failure: Some(reason)`: it is scored and
reported as *known*, stays out of the pass rate (and `--min`), and the report says when it passes
again so the mark can come off. Nine cases are marked today (the first run against Premiere Pro
26.5.2, 2026-10-16): the five sound cases (our FCP7 XML writes a stereo clip as one clip item, which
Premiere plays as mono at −3 dB), Dip to Black (Premiere imports it as a Cross Dissolve), Cross
Dissolve (Premiere blends in gamma-encoded space), Gaussian Blur and Brightness & Contrast (just
under 35 dB). Each is one fix in `interchange` or `render`, then one line here.

## Reading a render

`reference::locate` accepts, per case directory, numbered stills (PNG, TIFF, BMP, JPEG, Targa,
DPX, OpenEXR; ordered by the trailing number of the name) or one movie we decode (ProRes / H.264
/ DNxHR in MOV, MP4, MXF…), plus a WAV / AIFF (which wins over the movie's own sound). Alpha is
flattened over black. Our own render is read through the same path, so both sides see the same
decoders. Limits: 20 000 frames and one hour of audio per render.

## Tests

`cargo test -p filmcraft-fidelity` (0.4 s): analytic scores (identical frames, a known MSE,
percentiles with an outlier, white vs black ΔE = 100, the CIEDE2000 reference pairs, a delayed
copy found at its lag in both directions, a 1 234-sample lag through the FFT path, the FFT
cross-correlation against brute force, a known SNR, silence and empty inputs), the reference
reader on real directories (ordering, hidden and foreign files, a corrupt movie is an error),
the manifest and protocol text, argument parsing, the verdict rules, and the whole pipeline on
two cases at 160×90: no reference, our own render as a perfect reference (every score ∞ / 0,
every case passes), then a perturbed reference (one frame reddened, the sound halved: the right
frame is named, SNR is 0 dB, the picture case fails and the sound case, which is marked known,
fails as *known*), then a corrupt reference (that case fails, named; the run and its report
complete).

## Limitations

- The tolerances are provisional: the first full run (2026-10-16, Premiere Pro 26.5.2) put the
  baseline `cut` at PSNR 47.8 dB, mean ΔE₀₀ 0.14, p95 0.69, and 11 of 20 cases pass; the other
  nine are marked known failures (above) until their fixes land.
- FCP7 XML carries Motion, Opacity, Time Remap, Audio Levels / Pan and transitions, not standard
  effects, so five cases need hand work in Premiere per run; effects with keyframes, masks, blend
  modes, Lumetri and audio effects are not in the corpus yet (each is one `CaseDef`).
- Scores are over the final 8-bit sRGB output: a difference in the colour pipeline shows up in
  every picture case; read them against the baseline.
- Sound is compared at 48 kHz stereo; a 5.1 or 96 kHz render would be resampled / truncated to
  that before scoring.
