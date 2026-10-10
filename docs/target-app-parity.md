# Parity with Adobe Premiere Pro

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-10 · **Change:** major (full re-measure against Premiere Pro 2026 26.5.2; replaces the scorecard and estimate that lived in ROADMAP.md) · **Target:** Adobe Premiere Pro 2026 (26.5.2, macOS)

The authoritative assessment of how close FilmCraft is to Premiere Pro, and how much work is left.
[ROADMAP.md](../ROADMAP.md) summarizes it; [gaps.md](gaps.md) lists every shortfall one at a time.
The deep checklists are [file-format-parity.md](file-format-parity.md),
[codec-parity.md](codec-parity.md), [hardware-parity.md](hardware-parity.md),
[ui-parity.md](ui-parity.md) and [localization-parity.md](localization-parity.md).

## Headline

| | Value | Kind |
|---|---|---|
| **Feature breadth** | **~86%** | Partly measured: menus 92% (label diff, below), effects / transitions / audio effects 100% (unit tests); the other areas are estimated |
| **Ready for real work** | **~55%** (range 50–60%) | Estimated, from the evidence below; weights written down |
| **Stage** | **alpha** | ~20 points and ~400–700 h from beta, and beta also needs `.prproj` import ([why](#stage)) |
| **Remaining to beta** | **~400–700 Opus 5.5 agent-hours** | Estimated, calibrated below |
| **Remaining to full parity** | **~1,550–2,750 Opus 5.5 agent-hours** | Estimated; ~70% parallelizes |
| Codebase (measured 2026-10-10) | 45 crates + 3 apps, ~343,000 lines of Rust, 2,520 `#[test]` functions, 1,098 commits, 268 merged PRs, 69 authors | `git`, `gh`, `grep` |

The previous estimate (2026-10-05) was breadth ~87%, ready ~50–60%, and "~210–325 agent-hours to
100%". The breadth figure is one point lower now because this pass counts the file formats Premiere
lists and we lack (AVI, WMV, camera RAW, EXR / DPX / PSD stills, `.prproj`, DVD / Blu-ray / DCP
export) rather than only the formats we have. The hours went up because the old figure covered the
checklist only and left out plugins, AI, localization, `.prproj` and the depth work that 188 open
issues now document. That is new evidence, not drift: see [Calibration](#calibration).

## What we measured against, and how

- **Target:** Adobe Premiere Pro 2026, version **26.5.2** (Spotlight `kMDItemVersion` of the
  installed `/Applications/Adobe Premiere Pro 2026`), plus Adobe Media Encoder 2026 for export.
- **Menus (measured).** The maintainers' native menu-bar dump of Premiere 26.5.2
  (`plan/premiere/menus.json`, taken 2026-09-30 through System Events; local only, never
  committed) has 433 leaf items. Removing Adobe-cloud, account, help, macOS system and
  recent-file entries leaves 375. A label match against the string literals of `crates/engine`,
  `crates/ui-egui`, `crates/edit` and `crates/project` finds **338 of 375 (90.1%)**; of the 37
  misses, 9 are dynamic or system items (About, AutoFill, Dictation, Emoji, window names), so
  in-scope coverage is **338 / 366 ≈ 92%**. Real misses: Spelling (2), Render and Replace /
  Restore Unrendered, Enhance Speech toggle, Auto-Tag Audio Types, Auto Reframe Sequence, Generate
  / Reassociate Source Clips, Project Shortcut, Refresh All Projects, Export Selection as Premiere
  Project, 5 workspaces (Essentials, Metalogging, Prelude, Starter, Text-Based Editing),
  Audio Clip / Track Effect Editor, Sequence Index, AI Models and Collaboration settings, UXP /
  Exchange plug-in entries. A label match is presence only, and can over-count (a label that
  exists in a different place); it is a lower bound on misses, not proof of behaviour. The script
  is ad hoc; turning it into `cargo xtask parity` is gap [G7](gaps.md#g7-no-measured-parity-tool-in-ci).
- **Effects (measured by tests).** `premiere_26_video_effects_catalogue`,
  `premiere_audio_set_is_complete_and_foldered` and `premiere_26_transition_tree` check the
  catalogue against Premiere's Effects panel: video effects 93/93 (+Legacy, Obsolete), audio
  effects 53/53, video transitions 84/84 (+21 Legacy), audio transitions 3/3.
- **The installed app (file names and listings only, per [AGENTS.md](../AGENTS.md) §2; nothing
  inside the bundle was opened or read).** `Contents/Resources/*.lproj`: de, en, es, fr, it, ja,
  ko, pt, ru, zh_CN (10 UI languages). `Contents/MediaIO/systempresets/` folder names are
  four-character exporter / codec codes: JPEG, QuickTime, MP3, PNG, Windows Media, AIFF, AVI,
  BMP, DPX, TIFF, Targa, WAV, HEVC, MPEG-2 / DVD / Blu-ray, MXF (JPEG 2000, DNx, P2, XDCAM HD,
  AS-10, AS-11), MP4 / AAC / H.264 / H.264 Blu-ray, FLV, uncompressed AVI, DCP, GIF, raw PCM,
  OpenEXR. `Contents/Frameworks` names show what it decodes natively: ARRIRAW (ARRI Image SDK),
  RED R3D, Sony RAW, Canon RAW (`CrmSdk`), Codex HDE, DNx SDK, JPEG XS, JPEG 2000 (Kakadu),
  DV100, MPEG-4 Part 2, OMF (`Pro4OMF`), AAF, LTC timecode, C2PA content credentials, and an ONNX
  runtime for its AI features. `PlugIns` lists OpenEXR, Radiance and RED / MXF XMP handlers.
- **Our side:** this repository on `origin/main` at `98ba9ac` (2026-10-10): code, crate READMEs,
  tests, `docs/performance.md` benchmarks, and the 188 open / 118 closed GitHub issues (user
  reports from releases 0.2–0.5).
- **Public documentation:** Adobe's "What's new" and release notes for 26.0–26.3 (Object Mask,
  redesigned mask tools, R3D NE, OpenTimelineIO, GPU thumbnails, 4:2:2 10-bit hardware decode on
  NVIDIA Blackwell), and Adobe's published lists of supported file formats and GPU / hardware
  acceleration.
- **Not done in this pass:** running Premiere side by side on shared projects. No rendering or
  behaviour of ours is compared with Premiere's output automatically yet ([G1](gaps.md#g1-nothing-compares-our-output-with-premieres)).

## Weights

Feature areas carry 70% of "ready for real work", cross-cutting qualities 30%. Weights reflect
what a working editor uses daily: the timeline first, then media, effects and panels.

| Group | Weight |
|---|---|
| Editing, timeline, trimming, multicam | 20% of features |
| Effects and transitions | 12% |
| Media import and codecs | 12% |
| Audio | 10% |
| Panels and UI fidelity | 10% |
| Colour | 8% |
| Graphics, titles, captions | 8% |
| Export and delivery | 8% |
| Project files and interchange | 8% |
| Preferences and project management | 4% |
| *Cross-cutting:* hardware and performance | 10% of overall |
| Stability | 8% |
| Platforms | 5% |
| Plugins and ecosystem | 5% |
| AI features | 2% |

Feature breadth weights the same ten feature areas only (cross-cutting qualities have no
"exists / doesn't" count).

## By feature area

Breadth = does it exist. Ready = depth, correctness, fidelity, robustness. Hours = Opus 5.5
agent-hours to close the area to Premiere, sequential.

| Area | Weight | Breadth | Ready | Hours | Evidence | Detail |
|---|---|---|---|---|---|---|
| Editing, timeline, trimming, multicam | 20% | ~90% | ~70% | 80–140 | Edit algebra, trim modes, Trim Monitor, multicam with audio sync, Premiere default keyboard (143 commands), Source playback and range dragging (10-08). Open: ripple-delete of a gap by column (#668), Backspace on gaps (#648), clip stretching (#653), deselect on empty space (#683), new tracks by dragging media (#483), Escape cancelling drags (#580), time remapping (#510), effects on a whole track (#623) | [ui-parity.md](ui-parity.md) |
| Effects and transitions | 12% | 100% (measured) | ~60% | 80–140 | Every effect exists; Warp Stabilizer is 2-D, Morph Cut and Auto Reframe approximate, optical flow renders as frame blending. 34 effect ids run on the GPU against ~82 of Premiere's ~92 marked Accelerated. No Object Mask (26.0, AI). Transform shutter angle adds no motion blur (#415); Ultra Key setting (#458) | [hardware-parity.md](hardware-parity.md) |
| Media import and codecs | 12% | ~75% | ~60% | 150–250 | Own bit-exact decoders for H.264, HEVC, VP9, AV1, ProRes, DNx, APV, MPEG-2, AAC, AC-3, Opus. But H.264 only 8-bit 4:2:0 and HEVC no 4:2:2 in software (most 10-bit cameras, #626); no camera RAW (R3D, ARRIRAW, BRAW, Sony RAW, Canon RAW, ProRes RAW, #345, #383); no E-AC-3 (#647), AVI (#598), WMV, DV, XAVC / P2 metadata; Sony start timecode (#460); WebM black (#432) | [codec-parity.md](codec-parity.md), [file-format-parity.md](file-format-parity.md) |
| Audio | 10% | ~88% | ~65% | 50–90 | Mixer graph, automation, all 53 effects, Essential Sound, 5.1, voice-over, Remix, multi-stream container audio (#361). Device and playback reports: crackle / cut-outs (#336, #410, #602, #605, #617); Enhance Speech is DSP, not a model; ALSA fallback fixed (#254) | |
| Panels and UI fidelity | 10% | ~88% | ~60% | 120–200 | All main panels. Missing: on-screen transform handles in the Program monitor (#639), Effect Controls keyframe zoom / divider / row lines (#640–#645), docking rearrangement (#493), multi-monitor windows (#466), filmstrip thumbnails (#614), marquee in Project panel (#578) | [ui-parity.md](ui-parity.md) |
| Colour | 8% | ~90% | ~70% | 40–70 | Lumetri complete incl. scopes, LUTs, colour management, HDR export. Lumetri on the GPU covers basic, creative and vignette only; HDR-aware Lumetri maths, macOS EDR display, D-Log M; a Lumetri report (#628) | |
| Graphics, titles, captions | 8% | ~85% | ~65% | 50–90 | Text engine with shaping and bidi, shapes, gradients (#514), our `.fcgt` templates (`.mogrt` refused by design), rolls / crawls, all caption formats. Arabic titles reported broken (#395); no 608/708 embedding in the video stream; speech-to-text off in release builds | |
| Export and delivery | 8% | ~80% | ~65% | 60–110 | H.264 (own encoder), HEVC through hardware only, ProRes, DNxHR, APV, MXF OP1a / OP-Atom, image sequences, GIF, WAV / AIFF, presets, queue, GPU export rendering (#421). Missing: AV1, MPEG-2 / DVD / Blu-ray, DCP, JPEG 2000 / XS MXF, AS-10 / AS-11, XDCAM HD, EXR / DPX / Targa sequences, MP3, AAC-only audio, ProRes 4444 in the UI (#342), interlaced, smart render; bitrate target missed (#371) | [file-format-parity.md](file-format-parity.md) |
| Project files and interchange | 8% | ~70% | ~45% | 90–160 | `.fcproj` with migrations, auto-save, recovery; FCP7 XML, FCPXML, AAF, OMF, EDL, OTIO, ALE. **Premiere's own `.prproj` cannot be opened** (the main beta blocker); AAF / OMF never validated in Media Composer / Pro Tools; XML import of a Premiere sequence took 97 s (#461) | [file-format-parity.md](file-format-parity.md) |
| Preferences and project management | 4% | ~80% | ~65% | 20–40 | 16 Settings categories, most wired; Project Manager, proxies, Link Media; Media Manager limits (#411); images as proxies (#451) | |
| **Features, weighted** | 100% | **~86%** | **~64%** | **740–1,290** | | |

## By dimension

| Dimension | Ready | Hours | Doc |
|---|---|---|---|
| Features (editing, effects, colour, audio, graphics, export settings, preferences) | ~66% | 350–620 | this file |
| UI / UX fidelity | ~60% | 120–200 | [ui-parity.md](ui-parity.md) |
| File formats (containers, project, interchange, captions, stills) | ~60% | 140–240 | [file-format-parity.md](file-format-parity.md) |
| Codecs (decode and encode, fidelity, camera media) | ~60% | 150–250 | [codec-parity.md](codec-parity.md) |
| Hardware acceleration (decode, encode, GPU effects, I/O devices) | ~45% | 120–200 | [hardware-parity.md](hardware-parity.md) |
| Localization (12 key languages) | ~35% of Premiere's set; 4 of 12 key languages near-complete | 150–230 | [localization-parity.md](localization-parity.md) |
| Performance (beyond hardware) | ~55% | 60–120 | [performance.md](performance.md) |
| Stability | ~45% | 60–120 | [gaps.md](gaps.md#g4-crashes-at-start-up-and-no-test-ci-on-pull-requests) |
| Platforms | ~60% | 50–90 | [gaps.md](gaps.md#g9-windows-and-linux-at-run-time) |
| Plugins and ecosystem | ~5% | 200–400 | [gaps.md](gaps.md#g11-no-plugin-hosting) |
| AI features | ~25% | 150–300 | [gaps.md](gaps.md#g12-ai-features) |
| **Total** | **~55%** | **~1,550–2,750** | |

Dimension hours are disjoint (the feature row excludes UI, formats and codecs), so they add up.
Evidence for the cross-cutting rows:

- **Hardware ~45%.** Hardware decode exists on all three desktop OSs (VideoToolbox; Media
  Foundation / D3D11 incl. VP9 and AV1; VA-API and NVDEC for H.264 / HEVC on Linux). Hardware
  encode: VideoToolbox H.264 / HEVC 8-bit; NVENC H.264 (Windows, Linux) and HEVC incl. Main 10 HDR
  (Windows). Missing against Premiere: Intel Quick Sync and AMD encoders, 10-bit HEVC on macOS,
  ProRes hardware, zero-copy upload, ~48 of Premiere's accelerated effects, external video I/O,
  control surfaces. Detail: [hardware-parity.md](hardware-parity.md).
- **Performance ~55%.** 4K H.264 / HEVC play with no dropped frames with hardware decoding (M4
  Pro); GPU export is 2.4–3.2× faster on effects-heavy timelines; AV1 software decode is still
  slow and 8K is not real time. Users on 0.5.0 report constant lag and ~100% CPU (#523, #525) and
  a CPU fallback adapter rendering everything in software (#632).
- **Stability ~45%.** Never-crash rules enforced by lints; hostile-input fuzzing. But open
  launch crashes on macOS 12 (missing VideoToolbox symbol, #468 / #661, #655), Intel UHD graphics
  (#512), Windows 11 (#380, #582), a generic "keeps crashing" (#687), and **no workflow runs the
  test suite on pull requests** (`.github/workflows` has release, packaging lint, FreeBSD and
  Windows ARM64 builds only); `cargo xtask ci` runs locally only.
- **Platforms ~60%.** Releases for macOS, Windows (x64, ARM64), Linux (AppImage, Flatpak, deb,
  tar) and FreeBSD; Linux is something Premiere doesn't run on at all. Runtime testing is still
  mostly macOS; Windows and Linux reports dominate the issue list. The web build runs but its shell
  is incomplete. No mobile.
- **Plugins ~5%.** No VST3 / Audio Units, no OpenFX, no Premiere / After Effects plug-in API, no
  UXP panels (#325). Needs an owner decision (FFI). MCP, CLI and the control channel are ahead of
  Premiere's scripting.
- **AI ~25%.** Speech to text exists (local Whisper) but is off in release builds; text-to-speech
  narration with Kokoro-82M landed 10-10 (#278, beyond Premiere); scene edit detection; Enhance
  Speech and Auto Reframe are DSP / heuristics. Missing: Object Mask (#406, #694), Generative
  Extend, media-intelligence search, caption translation, auto colour.

## Stage

**Alpha.** Core workflows run end to end (import, edit, trim, colour, mix, caption, deliver,
round-trip through XML / AAF), but ready-for-real-work is ~55%, below the ~75% beta bar, and the
standard's beta condition "opens and saves the target's main format reliably" fails outright:
FilmCraft cannot open a `.prproj` (it reads Premiere's FCP7 XML / AAF / OTIO exports instead).
Distance to beta: ~20 points of ready-for-real-work plus `.prproj` import, ~400–700 agent-hours:

| To reach beta | Hours |
|---|---|
| `.prproj` import of mainstream projects (sequences, clips, effects' common parameters, markers) | 60–120 |
| Stability: launch crashes, PR CI running `cargo xtask ci` on all three OSs, crash triage | 40–80 |
| H.264 High 10 / 4:2:2 and HEVC 4:2:2 software decoding (camera media) | 30–50 |
| Real-world media corpus and scripted workflow acceptance tests | 40–70 |
| UI fidelity top issues (Program monitor handles, Effect Controls, docking, deselect) | 60–100 |
| Hardware: zero-copy, VideoToolbox Main 10, Quick Sync / AMF encode, more GPU effects | 60–100 |
| Windows / Linux runtime fixes, audio device issues | 30–60 |
| Container gaps users hit: E-AC-3, AVI, multi-track audio, still formats | 30–50 |
| Interchange validation (AAF / OMF in Avid and Pro Tools, XML speed) | 20–40 |
| Performance: lag / CPU reports, AV1 speed | 30–60 |
| **Total** | **~400–730** |

## Calibration

Hours are Opus 5.5 agent wall-clock hours, calibrated against this repo's history (`git log`,
`gh pr view` timestamps):

- **Whole history.** First commit 2026-09-30; by 2026-10-10, 1,098 commits and 268 merged PRs
  (243 since 10-05, most from community contributors' agents) took breadth from 0 to ~86%. The
  2026-10-01/02 blocks measured **~4–5 agent-hours per checklist point** with five parallel
  agents (old ROADMAP).
- **A hardware decode backend** (one OS API, H.264 + HEVC, with parity tests): VideoToolbox #33
  (first commit 07:25, merged 11:26 the same day), VA-API #358 (+3,942 lines), NVDEC #549 (06:17 →
  11:04): **~3–5 h each**.
- **A hardware encoder per codec and OS:** NVENC HEVC #328 (+1,635 lines, 19:28 → 13:46 next day
  including review), Main 10 HDR #360 (02:38 → 14:05): **~4–8 h each**.
- **GPU export rendering plus four GPU effects** (#421, 15 commits over ~23 h): **~8–12 h**.
- **A whole-interface language** (~3,500 strings, tests): Japanese #449 in one commit:
  **~4–8 h**, plus native review (human). A menus-only language (Ukrainian #407, ~350 strings):
  ~1 h.
- **A clean-room decoder from spec, bit-exact**: AV1 (09-30 → 10-01 night, several agents): ~15–30 h.
- **Depth work costs more per point than breadth.** Each remaining point of ready-for-real-work
  is a bug class, a format edge or a fidelity comparison rather than a missing menu item; the
  estimates apply ~1.5–2× the breadth rate.

- **Wall-clock limits** (from the 2026-10-02 blocks): more than ~5 local agents slows everyone
  (each worktree build is 6–7 GB; a full `cargo xtask ci` takes 20–60 min under load; the disk
  filled twice), and one integrator merging and re-running CI is a bottleneck. With 4–5 parallel
  agents plus an integrator, wall-clock is ~¼–⅓ of the agent-hours; community PRs now add
  capacity beyond that.

What parallelizes: almost everything by area (~70%). Serial: `.prproj` import before beta,
fidelity harness before fidelity numbers. **Needs a human:** the plugin-hosting decision (FFI,
licences), native-speaker review of every language, licensed or trained models for AI features,
hardware we don't own (Intel Quick Sync, AMD encoders, Blackwell 4:2:2, DeckLink / AJA I/O,
control surfaces), and validation in Avid Media Composer / Pro Tools.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | major | Created from ROADMAP.md's scorecard and estimate; full re-measure against Premiere Pro 26.5.2 (menu label diff, installed-bundle listings, 188 open issues); breadth ~87 → ~86% (file formats counted), ready ~50–60% → ~55%, hours re-estimated to include plugins, AI, localization and `.prproj`; stage alpha |
| 2026-10-05 | major | Honest assessment added in ROADMAP.md (checklist ~87% vs ready ~50–60%) |
| 2026-10-02 | minor | Scorecard ~62% → ~72% → ~87% as the menu long tail, effects and codecs landed |
