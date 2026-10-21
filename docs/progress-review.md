# FilmCraft progress review — 2026-10-21

> **Last reviewed:** 2026-10-21 · **Last updated:** 2026-10-21 · **Change:** major (source audit of the twenty tracked gaps) · **Target:** Adobe Premiere Pro 2026 (26.5.2)

This review checks the local FilmCraft **0.10.0** checkout at **`d643a886`** against the
2026-10-10 assessment. Read it before choosing work from [gaps.md](gaps.md).
The comparison target remains the version used in that assessment; it was not inspected again.

## What this review proves

Source files, test bodies, manifests, workflow declarations and local Git history were inspected.
The translation catalogs were counted again. A follow-up checked shared GitHub workflow sources
and selected existing run results, as recorded below. **No application sessions, benchmarks,
Premiere comparisons or new GitHub workflow runs were executed for this review.** The required
`cargo xtask ci` command was attempted but could not start because Cargo is unavailable in this
environment; no Rust tests ran. A test linked here means coverage exists in source, not that it
passed on this machine today. Issue numbers elsewhere are historical references; their current
GitHub status was not checked.

**86% breadth, 56% ready for real work, 55% mainstream and 60% essentials remain historical
2026-10-10 estimates.** They are not current measurements of 0.10.0. The old hours are also
historical planning estimates, not newly assessed remaining work. This audit neither raises those
numbers nor certifies beta readiness.

## Review of all twenty gaps

| Gap | Source finding on 2026-10-21 | Remaining work / evidence needed |
|---|---|---|
| G1 — fidelity against Premiere | [Golden tests](../crates/golden/tests/golden.rs) compare FilmCraft output; no Premiere-output comparison harness was found in the inspected test and xtask sources. | Shared original projects rendered by both applications, with frame/audio comparisons. |
| G2 — `.prproj` | [Interchange formats](../crates/interchange/src/lib.rs) do not include `.prproj`; a search of `crates/` and `apps/` found no `prproj` implementation. | Importer, fixtures, unsupported-feature reporting and round-trip/acceptance checks. |
| G3 — camera profiles | [H.264 limits](../crates/h264/README.md) still say 8-bit 4:2:0 only; [HEVC limits](../crates/hevc/README.md) still exclude 4:2:2 / RExt. | Software profile support and conformance fixtures; hardware support does not close the software gap. |
| G4 — launch / PR test CI | [Graphics fallback](../apps/filmcraft/src/graphics.rs) is implemented. The five local [workflow declarations](../.github/workflows) delegate to shared workflows. FreeBSD explicitly skips tests; the selected Windows ARM64 run checks packaging/install/CLI startup. No general PR Rust-test gate is declared locally. | Reproduce historical launch reports and add or verify full Rust gates on all three desktop OSs; packaging checks alone do not establish this coverage. See the GitHub evidence below. |
| G5 — acceleration | [Platform registration](../crates/platform/src/lib.rs) still uses VideoToolbox, Media Foundation, VA-API/NVDEC and NVENC paths. | Zero-copy, additional encoders/effects and hardware-specific runtime checks remain to be assessed. |
| G6 — workflow/corpus testing | Two [CLI workflows](../apps/filmcraft-cli/tests/workflows.rs) already cover synthetic VFR MP4 and long-GOP multi-audio MKV, editing, export and reopening. | Run them with required oracles; add a pinned real-media corpus. Do not describe all workflow tests as absent. |
| G7 — parity tooling | [The xtask tool](../xtask/src/parity.rs) supports menus, effects and transitions, reference files, placement and a minimum threshold. | Other areas, committed references/checklist and verified CI enforcement. Presence is not rendering fidelity. |
| G8 — interface | Transform handles, panel merge/split/float/redock, Effect Controls divider/row lines, Position curves, empty-space deselect and gap deletion are implemented. See the evidence below. | Keyframe zoom, remaining interaction details and independent visual/platform validation. This is a partial gap, not a list of wholly missing controls. |
| G9 — desktop platforms | [Startup/file handling](../apps/filmcraft/src/main.rs) and Windows-specific tests exist. | Fresh Windows/Linux acceptance sessions and reproduction of historical reports; packaged builds alone do not establish runtime quality. |
| G10 — imports | [Codec registry](../crates/codecs/src/lib.rs), [DNG](../crates/dng/README.md) and [media code](../crates/media/src/lib.rs) already include additions described in G10, including E-AC-3 and CinemaDNG paths. | Remaining profiles, containers and camera RAW formats; rerun import fixtures before claiming fidelity. |
| G11 — plugins | No VST3/AU/OpenFX host was found in the inspected workspace manifests and engine sources. | Host architecture/owner decision and integration tests. |
| G12 — AI | [Desktop features](../apps/filmcraft/Cargo.toml) expose optional Whisper and neural voices. | Model-based feature coverage, model provisioning and validation; compile-time options alone do not prove release availability. |
| G13 — delivery | [Export registry](../crates/export/src/lib.rs) includes FLAC, ProRes 4444/XQ paths and image sequences. AV1 has format/mux plumbing, but no built-in encoder or platform AV1 probe was found. | AV1 backend and other delivery gaps; a format enum entry must not be counted as a usable encoder. |
| G14 — languages | Nine non-English catalogs are near the current catalog-key union; Russian and Ukrainian are partial. Korean is implemented. Counts below. | Hindi/Arabic/Vietnamese catalogs, RTL interface work, untranslated source/engine text and native review. |
| G15 — interchange validation | [AAF/OMF tests](../crates/interchange/tests/aaf_omf.rs) and [XML tests](../crates/interchange/tests/fcp7.rs) exist. | External Avid/Pro Tools acceptance and new XML-import timings. The old 97 s / 66 s report was not reproduced. |
| G16 — performance | AV1/VP9 parallelism is documented in their crates and [performance.md](performance.md); the old lag reports describe older releases. | Re-run representative playback/export budgets on identified hardware. No new speed claim follows from this audit. |
| G17 — audio reliability | [Audio implementation](../apps/filmcraft/src/audio.rs) includes recovery work; previous device reports remain historical evidence. | Device-loss, overlapping clips and real-interface sessions on Windows/Linux. |
| G18 — effect depth | [Multicam tests](../crates/ui-egui/tests/multicam_ui.rs) cover paging beyond sixteen angles and Edit Cameras; it is no longer a missing feature. | Stabilization/morph/mask fidelity and HDR behaviour still need focused review and comparison. |
| G19 — transcription packaging | Desktop and CLI default features are empty; both expose an optional `whisper` feature. The [release wrapper](../.github/workflows/release.yml) delegates the build externally. | Check the shared build's feature flags and shipped binaries before asserting transcription is off in every release. |
| G20 — browser | [Web app](../apps/filmcraft-web/src/lib.rs) includes file access, WebCodecs, WebAudio, OPFS recovery and cooperative export; a [browser smoke script](../apps/filmcraft-web/tests/smoke.mjs) exists. | Run the browser script; validate downloads and recovery. Thread-dependent commands and broad browser/media acceptance remain limited. |

## UI evidence that changes the work list

| Previously listed as missing | Implementation / test source inspected |
|---|---|
| Motion / Transform monitor handles | [transform_handles.rs](../crates/ui-egui/src/panels/transform_handles.rs); [move/scale/rotate/anchor and edge tests](../crates/ui-egui/tests/transform_handles_ui.rs) |
| Rearranging panels | [dock_shared.rs](../crates/ui-egui/src/dock_shared.rs); [merge/split/float/redock and persistence tests](../crates/ui-egui/tests/shared_docking_ui.rs); [resize tests](../crates/ui-egui/tests/ui_dock_resize.rs) |
| Effect Controls divider and row lines | [effect_controls.rs](../crates/ui-egui/src/panels/effect_controls.rs); `effect_controls_divider_and_scrolling_anywhere` in [keyframes_ui.rs](../crates/ui-egui/tests/keyframes_ui.rs) |
| Position value/velocity graphs | [effect_controls.rs](../crates/ui-egui/src/panels/effect_controls.rs); local commit `5a243ea8` |
| Empty-space deselect | [deselect_ui.rs](../crates/ui-egui/tests/deselect_ui.rs), covering timeline and monitor |
| Gap delete / Backspace | [timeline_gap_ui.rs](../crates/ui-egui/tests/timeline_gap_ui.rs) |
| Continuous timeline filmstrip | Local commit `c85d0ac9`; [timeline drawing](../crates/ui-egui/src/panels/timeline.rs) and [timeline scroll/display tests](../crates/ui-egui/tests/timeline_scroll_ui.rs) |
| Monitor Button Editor | [monitor.rs](../crates/ui-egui/src/panels/monitor.rs) contains visibility/reset UI and `button_editor_updates_visibility_without_affecting_other_monitor` |
| Multicam beyond sixteen angles | `multicam_view_pages_beyond_sixteen_angles_and_edit_cameras` in [multicam_ui.rs](../crates/ui-egui/tests/multicam_ui.rs) |

Floating groups in the app are not proof of separate native windows on additional monitors.
Likewise, these implementations do not establish exact Premiere interaction fidelity.

## Recounted localization evidence

The denominator is **3,597 unique `(context, source)` keys** in the union of the eleven shipped
TSV catalogs. Count nonempty, non-comment lines with exactly three tab-separated columns and
nonempty source/translation; deduplicate by context/source. All eleven catalogs had zero malformed
rows in this check. This measures catalog presence, not translation quality, engine messages,
unwrapped UI literals, font availability or RTL layout. English is the source language, not a TSV
catalog. Percentages below are rounded to one decimal.

| Catalog | Unique keys | Share of union |
|---|---:|---:|
| German (`de`) | 3,590 | 99.8% |
| Spanish (`es`) | 3,596 | 100.0% |
| French (`fr`) | 3,564 | 99.1% |
| Indonesian (`id`) | 3,559 | 98.9% |
| Italian (`it`) | 3,593 | 99.9% |
| Japanese (`ja`) | 3,593 | 99.9% |
| Korean (`ko`) | 3,594 | 99.9% |
| Portuguese, Brazil (`pt-br`) | 3,559 | 98.9% |
| Russian (`ru`) | 355 | 9.9% |
| Ukrainian (`uk`) | 355 | 9.9% |
| Simplified Chinese (`zh-cn`) | 3,579 | 99.5% |

## Next evidence to collect

1. Extend the selected GitHub checks below to full test coverage, release feature flags and shipped
   binaries; record tested revisions, platforms, required oracle settings and skipped tests.
2. Run the two CLI workflows with `FILMCRAFT_REQUIRE_ORACLES=1`, the UI tests linked above, and
   the browser smoke script. Record failures separately from unsupported cases.
3. Exercise launch → import → edit → title/audio → export → save/reopen on Windows and Linux.
4. Validate `.prproj` and camera-profile requirements against representative original projects
   and files before starting those implementations.
5. Re-measure performance and Premiere output fidelity, then recompute the readiness weights
   and remaining estimates together in the progress documents.

## GitHub workflow follow-up

These are read-only checks of upstream configuration and existing runs on 2026-10-21, not new CI
executions. FilmCraft `main` was still at `d643a886650bfbf16f24762274054575aa1e8f20`.

- The [FreeBSD caller at that revision](https://github.com/storytold/filmcraft/blob/d643a886650bfbf16f24762274054575aa1e8f20/.github/workflows/freebsd.yml)
  sets **`skip-tests: true`**. The shared workflow supports `cargo test`, but that capability is
  explicitly disabled by this caller. It schedules nightly builds and selected packaging PRs.
- [Windows ARM64 run 39587230356](https://github.com/storytold/filmcraft/actions/runs/39587230356)
  succeeded at source revision `a81a08e6a45ffae252d9de8835dc2985583673c4`. Its recorded shared
  workflow revision is `9824a77554c3e2c3b9e1094e97395fd23ab71624`; the
  [workflow source](https://github.com/storytold/craft-common-actions/blob/9824a77554c3e2c3b9e1094e97395fd23ab71624/.github/workflows/windows-arm64.yml)
  builds/packages the MSI, installs it, runs the CLI version check and uninstalls. This is useful
  packaging/startup coverage, not the workspace test suite or an interactive editing session.
- The [shared release workflow inspected](https://github.com/storytold/craft-common-actions/blob/9824a77554c3e2c3b9e1094e97395fd23ab71624/.github/workflows/release.yml)
  invokes platform packaging scripts and creates a draft release. This does not by itself verify
  optional transcription features in the shipped binaries.
- [PR #2458](https://github.com/storytold/filmcraft/pull/2458) reports local Rust/UI tests in its
  description; the recorded check list contains CodeRabbit. An author's test report and a review
  bot check should be distinguished from a centrally enforced Rust-test run.

The sample does not establish branch-protection requirements or historical test coverage for
every PR. The five current local wrappers are FreeBSD, Nix, Packaging lint, Release and Windows ARM64.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-21 | major | Audit of all twenty gaps against 0.10.0 at `d643a886`; UI/web corrections, fresh catalog counts, selected GitHub workflow follow-up and explicit limits on historical estimates and unexecuted tests |
