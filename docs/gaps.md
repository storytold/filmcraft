# Where FilmCraft falls short of Premiere Pro

> **Last reviewed:** 2026-10-21 (source audit) · **Last updated:** 2026-10-21 · **Change:** minor (correct implemented UI/web features; distinguish historical reports and estimates from source evidence) · **Target:** Adobe Premiere Pro 2026 (26.5.2)

**Read the [2026-10-21 progress review](progress-review.md) first.** It checks all twenty gaps
against 0.10.0 at `d643a886`, with source/test links. Tests were inspected, not executed. Issue
references and hours below come from the 2026-10-10 assessment unless a later entry says otherwise;
they are not a fresh issue-status check or remaining-effort estimate. Percentages have not been
re-measured. The progress review records a selected GitHub workflow follow-up; full test coverage
and shipped release features still need validation.

Every known shortfall, one entry each, ranked by what most stops an editor using FilmCraft for real
work. This is the work list: agents should prefer it over adding checklist items. Numbers are in
[target-app-parity.md](target-app-parity.md); estimates are Opus 5.5 agent-hours, calibrated there.
When you close or shrink a gap, update its entry, its parity doc and the revision history.

Summary by kind: **feature gaps** G1, G2, G12, G13, G18, G19; **UI / UX** G8; **file formats**
G2, G10, G13, G15; **codecs** G3, G10; **hardware** G5; **localization** G14; **stability and
platforms** G4, G9, G17; **performance** G16; **ecosystem** G11; **measurement** G1, G6, G7;
**web** G20.

| # | Gap | Kind | Blocks beta | Estimate |
|---|---|---|---|---|
| G1 | Nothing compares our output with Premiere's | measurement | no, but every ready number depends on it | 40–70 h |
| G2 | Premiere's `.prproj` cannot be opened | file format | **yes** | 60–120 h |
| G3 | 10-bit and 4:2:2 camera media needs hardware | codec | yes | 30–50 h |
| G4 | Historical launch crashes; PR test coverage needs verification | stability | yes | 40–80 h |
| G5 | Hardware acceleration is partial | hardware | partly | 120–200 h |
| G6 | No pinned real-media corpus; synthetic workflow tests exist | measurement | yes | 40–70 h |
| G7 | No measured parity tool in CI | measurement | no | 10–20 h |
| G8 | Remaining UI fidelity and validation; handles/docking implemented | UI / UX | partly | 120–200 h |
| G9 | Windows and Linux at run time | platforms | partly | 50–90 h |
| G10 | Container and codec gaps on import | file format, codec | partly | 120–200 h |
| G11 | No plugin hosting | ecosystem | no | 200–400 h |
| G12 | AI features | AI | no | 150–300 h |
| G13 | Delivery formats | file format | no | 60–110 h |
| G14 | Localization: 3 of the 12 key languages missing | localization | no | 150–230 h |
| G15 | Interchange never validated in Avid / Pro Tools; slow XML import | file format | partly | 20–40 h |
| G16 | Performance reports on 0.5.0 | performance | partly | 30–60 h |
| G17 | Audio device and playback reliability | stability | partly | 20–40 h |
| G18 | Effects depth: approximations and missing behaviour | feature | no | 60–110 h |
| G19 | Transcription optional; shipped release features unverified | feature, AI | no | 5–10 h |
| G20 | Browser limitations and acceptance validation | platforms | no | 40–80 h |

## G1. Nothing compares our output with Premiere's

- **Missing:** a fidelity harness. Every effect, transition, Lumetri control, blend mode, keyframe
  interpolation and audio effect counts once it exists; nothing renders the same project in
  Premiere and FilmCraft and compares frames or samples. Effects described as approximations
  count the same as exact ones.
- **Evidence:** catalogue tests check names and folders only. EffectCraft's live After Effects
  comparison (its gaps.md G1) found real bugs in features marked done on its first run.
- **Impact:** "ready for real work" stays an estimate; colour and effect differences surface only
  when a user notices.
- **Done when:** a corpus of original test projects (our own media), opened in Premiere through
  FCP7 XML / OTIO, rendered by both, with per-effect PSNR / ΔE and audio-sample scores reproducible
  from one command; Premiere output stays local in `plan/premiere/` (gitignored). Owner runs
  Premiere (installed). **40–70 h.** Doc: [target-app-parity.md](target-app-parity.md).

## G2. Premiere's `.prproj` cannot be opened

- **Missing:** import of Premiere project files. `grep -ri prproj crates apps` finds nothing.
  Today an editor must export FCP7 XML, AAF or OTIO from Premiere first, losing bins, effects and
  most metadata; there is no "Selection as Premiere Project" export either.
- **Impact:** the beta bar requires opening the target's main format; every switching editor's
  existing work is behind this.
- **Approach:** `.prproj` is gzip-compressed XML. Clean-room from projects we author ourselves in
  Premiere (the owner has `plan/premiere/fixtures/filmcraft_ref.prproj`): sequences, tracks, clips,
  in/out, speed, transitions, markers, bins, labels, then the common effect parameters (Motion,
  Opacity, Lumetri basic, Volume). Report what was not imported. Export later. **60–120 h.**
  Doc: [file-format-parity.md](file-format-parity.md).

## G3. 10-bit and 4:2:2 camera media needs hardware

- **Missing:** our H.264 decoder is 8-bit 4:2:0 only (`crates/h264/README.md`: no High 10, High
  4:2:2, 4:4:4, lossless, interlaced field / MBAFF); HEVC has no 4:2:2 / RExt. Most mirrorless and
  cinema cameras record exactly these (Sony XAVC S-I / XAVC HS 4:2:2 10-bit, Panasonic, Canon
  XF-AVC, Fujifilm).
- **Evidence:** #626 (Panasonic Lumix 4K 10-bit 4:2:2 MOV). On macOS VideoToolbox decodes 10-bit
  4:2:2 H.264 / HEVC on chips that support it; Windows Media Foundation and Linux decline them, and
  there is no software fallback, so the clip fails. Premiere decodes them everywhere (and in
  hardware on Apple silicon, Intel and NVIDIA Blackwell).
- **Impact:** camera originals from most current cameras do not open on Windows or Linux.
- **Estimate:** H.264 High 10 + High 4:2:2 (chroma geometry, deblocking, transforms) 15–25 h;
  HEVC Main 4:2:2 10 (RExt subset) 15–25 h. Doc: [codec-parity.md](codec-parity.md).

## G4. Crashes at start-up, and no test CI on pull requests

**2026-10-21 source check:** the historical reports below were not reproduced or checked for
closure. Local workflow declarations delegate to `storytold/craft-common-actions` reusable
workflows. The FreeBSD caller sets `skip-tests: true`; the selected successful Windows ARM64 run
checks packaging/install/CLI startup. No general PR Rust-test gate is declared locally. See the
[pinned sources and recorded run](progress-review.md#github-workflow-follow-up); the sample does
not establish all historical runs or branch-protection requirements.

- **Have:** a graphics start-up fallback (#820): after two starts in a row that died within 15 s
  of launch without painting (a driver crash in wgpu instance / device creation; hangs, later
  crashes and concurrent instances don't count), the next launch moves on to DirectX 12 only
  (Windows), then a software adapter (WARP, lavapipe, llvmpipe) with CPU compositing, and keeps
  the mode that works; a start-up error wgpu reports moves on in the same process. A fallback mode
  shows a notice with Use Default Graphics; `--graphics default|dx12|software` picks a mode by
  hand. An explicit `WGPU_BACKEND` stays in charge. On Windows, a GPU that Vulkan lists through two
  installed versions of one driver (GPUs on different driver packages) draws with the newer one
  instead of dying inside the stale one (`filmcraft_gpu::adapter`).
- **Missing:** a launch that never fails, and CI that runs the gates.
- **Evidence:** open issues: macOS 12 Monterey aborts on a missing VideoToolbox symbol (#468,
  #661, #655), Intel UHD graphics (`igvk64.dll`, #512), Windows 11 does not run or fails after setup
  (#380, #582, #413, #477), crash on new project + import (#439), "it just keeps crashing" (#687).
  The 2026-10-10 assessment found no PR workflow running `cargo test` or `cargo xtask ci` and
  attributed the preceding 243 merges to contributors' local gates. That is historical evidence;
  see the source-check qualification above for the current shared workflow setup.
- **Impact:** a user who cannot start the app gets zero value; regressions land unnoticed.
- **Done when:** weak-linked or runtime-loaded OS symbols on old macOS, ~~a software / safe-mode
  start when the GPU adapter fails~~ (done, #820), a PR workflow running `cargo xtask ci`
  on macOS, Windows and Linux, and a crash-report triage pass. **40–80 h.**

## G5. Hardware acceleration is partial

- **Have:** hardware decode on macOS (VideoToolbox H.264 / HEVC), Windows (Media Foundation /
  D3D11: H.264, HEVC, VP9, AV1) and Linux (VA-API and NVDEC: H.264, HEVC); hardware encode
  VideoToolbox H.264 / HEVC 8-bit, NVENC H.264 (Windows, Linux) and HEVC incl. Main 10 HDR
  (Windows); GPU compositing, blend modes, 34 effect ids incl. Lumetri basic / creative / vignette,
  GPU export rendering (#421).
- **Missing:** zero-copy decoded frames into wgpu (every frame is read back to the CPU and
  re-uploaded); VideoToolbox Main 10 / HDR HEVC encode and B-frames; ProRes hardware decode /
  encode on Apple silicon; Intel Quick Sync and AMD AMF encoders (and their 4:2:2 decode);
  VA-API VP9 / AV1 and VA-API encode; HEVC encode on Linux; AV1 hardware encode; ~48 of Premiere's
  ~82 accelerated effects (keys, masks, Lumetri curves / wheels / HSL, distortions, transitions);
  external video I/O (DeckLink / AJA, Mercury Transmit), control surfaces (EUCON / Mackie);
  a CPU fallback adapter renders everything on the CPU (#632).
- **Estimate:** 120–200 h (hardware we don't own needs a human). Doc:
  [hardware-parity.md](hardware-parity.md).

## G6. No real-world media corpus or workflow acceptance tests

- **Done:** first scripted MCP workflows (#835, `apps/filmcraft-cli/tests/workflows.rs`): the
  `filmcraft-cli mcp` binary imports ffmpeg-made phone-like VFR H.264 + AAC MP4 and long-GOP
  H.264 Matroska with two audio tracks (AAC, AC-3), edits (extract, audio gain, title; ripple
  trims, colour matte, cross dissolve), exports H.264 MP4, and saves a project that a new process
  exports as ProRes; ffprobe / ffmpeg check streams, frame count, duration, frame placement, A/V
  sync, title pixels, dissolve mix and audio levels. Skipped without ffmpeg.
- **Missing:** a pinned corpus of phone, camera and screen recordings (variable frame rate,
  damaged files, long GOP, odd containers, multi-track audio), each with an import → edit → export
  round trip checked against an ffprobe / ffmpeg oracle; scripted MCP sessions that do real jobs
  end to end.
- **Evidence:** export stopped producing frames with iPhone HEVC clips on Linux (#480); WebM shows
  black (#432); only the first audio track imported (#397, #416, #601; multi-stream support landed
  in #361, reporters not yet confirmed); bitrate target produced 6.1 Mb/s for 40 (#371).
- **Done when:** corpus fetched pinned and sha256-verified (as PhotoCraft's `cargo xtask corpus`,
  craftrules `standards/test-corpora.md`), runs nightly, ≥ 50 files. **40–70 h.**

## G7. No measured parity tool in CI

- **Done:** `cargo xtask parity` measures menus (#781): our shipped menu bar (the tree the app
  builds, `examples/menu_tree.rs`) against a reference list of menu paths, by default the local
  `plan/premiere/menus.json`; presence and placement reported separately (same path, elsewhere,
  missing), app / Help / recent / account / macOS items out of scope, a markdown checklist, and
  `--min` to fail below a presence threshold. Without a reference it prints our inventory only.
  `--area effects` and `--area transitions` (#831) measure the Effects panel the same way: the
  effects and transitions the engine's effect definitions list (`examples/effect_catalog.rs` in
  `filmcraft-project`) against a reference of folder paths or bare names (default the local
  `plan/premiere/effects.json` / `transitions.json`), placement by folder, Presets and the other
  area out of scope.
- **Missing:** the other areas (commands, panels, preferences, formats), fidelity tiers (exact,
  approximate, stub), a CI job, and a committed `docs/parity-checklist.md`.
- **Evidence:** the 92% menu figure in this pass came from an ad hoc label-matching script; it has
  not yet been re-measured with the tool.
- **Estimate:** 8–17 h left (the menu snapshot is local-only, so CI needs a committed, Adobe-free
  list of menu paths for `--reference`, or the check runs only where `plan/` exists).

## G8. UI fidelity: monitor handles, Effect Controls, docking

- **Implemented, source checked 2026-10-21:** Motion / Transform monitor handles (#639), Effect
  Controls divider and row lines (#640, #643), Position value/velocity graphs (#448), panel
  rearrangement and floating groups (#493), empty-space deselect (#683), gap deletion with Delete
  or Backspace (#648, #668), and continuous timeline thumbnails (#614). The Button Editor has
  persisted monitor-button state; it should no longer be described as wholly absent without a
  focused runtime check. [Evidence and test sources](progress-review.md#ui-evidence-that-changes-the-work-list).
- **Remaining / needs validation:** keyframe zoom, Project-panel marquee/pan details, separate
  native multi-monitor windows (in-app floating groups do not establish this), transition A/B
  previews and Start / End sliders, platform interaction reports and independent visual comparison.
  Historical issues are reproduction candidates, not proof of a current bug.
- **Historical estimate:** 120–200 h, not recalculated after these implementations. Doc:
  [ui-parity.md](ui-parity.md).

## G9. Windows and Linux at run time

- **Have:** files given at launch (terminal, Open With, file associations) open whatever the case
  of `.fcproj` and are made absolute, and arguments that are not Unicode no longer stop the start;
  `filmcraft-cli import` stores no Windows `\\?\` paths (#850).
- **Missing:** routine runtime testing off macOS. Linux playback stutter (#617), Wayland title
  bar (#433), case-sensitive extensions (#593), "Report an issue" link (#642), Windows bins cannot
  be left (#587), Sequence Settings won't open on Windows (#586, #351), old hardware (#491).
- **Estimate:** 50–90 h (overlaps G4's CI work).

## G10. Container and codec gaps on import

- **Have:** E-AC-3 (Dolby Digital Plus) in MP4, MOV, Matroska and MPEG-TS (#647): independent
  substream, 1–6 blocks per syncframe, spectral extension. MP1 / MP2 (MPEG audio Layer I / II):
  own decoder (#809) in raw files, MP4 and MOV (#800), MPEG-TS / PS and Matroska (`A_MPEG/L1` and `L2`, #1402), including the
  bit rate / mode combinations ISO/IEC 11172-3 disallows but ffmpeg writes (its mono default is
  384 kbit/s). Radiance HDR (`.hdr`) stills and image sequences (#1407): RGBE as linear float, flat
  and run-length scan lines, any axis order. OpenEXR stills and image
  sequences (#828): scan-line, uncompressed / RLE / ZIPS / ZIP, half / float / uint, RGB(A) or
  luminance, decoded to linear float frames and bit-exact against ffmpeg; PXR24 (lossy 24-bit
  float, 16 lines per chunk) decodes too, bit-exact against OpenEXR (via OpenImageIO, #828).
- **Missing:** free-format MPEG audio, E-AC-3 adaptive hybrid
  transform, enhanced coupling and the extra channels of 7.1 dependent substreams (played as the
  5.1 core), AVI (#598; listed as an extension but no
  demuxer), WMV / ASF, DV / DVCPRO / DV100, MPEG-4 Part 2, FLAC in MP4 (#603), camera RAW (R3D
  incl. R3D NE, ARRIRAW, Sony RAW / X-OCN, Canon RAW, BRAW, ProRes RAW; #345, #383), JPEG 2000 /
  JPEG XS MXF, the rest of CinemaDNG (lossy / JPEG XL DNG, MXF-wrapped, opcode lists, Raw
  settings; DNG frames and folders import since #1065); stills: PSD, EXR beyond scan-line RLE / ZIP / PXR24 (PIZ, B44, DWA,
  tiled, multi-part, deep), HEIF / HEIC,
  AI / EPS, and the rarer DPX layout, run-length encoding (Targa and DPX stills and sequences import
  since #821, packed 10/12-bit and Y'CbCr DPX since #841, deeper than 8 bits kept at 8); transparent
  video (#402); Sony start timecode in `rtmd` (#460); stereo 3D flag (#500); MKV multi-audio (#601).
- **Estimate:** 120–200 h (camera RAW alone 60–100 h, and some SDKs have licences we cannot use:
  clean-room from public specs only where specs exist). Docs: [codec-parity.md](codec-parity.md),
  [file-format-parity.md](file-format-parity.md).
- **Have:** Matroska AAC / MP3 / AC-3 / E-AC-3 sample-aligned with the signal the encoder was
  given (the exact `CodecDelay` is trimmed, not the 1 ms-rounded one; #790).

## G11. No plugin hosting

- **Missing:** VST3 / Audio Units, OpenFX, Premiere / After Effects plug-in APIs, UXP panels
  (#325).
- **Impact:** many professional editors depend on third-party effects and audio plug-ins.
- **Needs:** an owner decision (FFI under the `unsafe` rule, AGENTS.md §0.3). **200–400 h.**

## G12. AI features

- **Missing:** Object Mask (Premiere 26.0, #406, #694), Generative Extend (needs a large video
  model; not reachable locally), media-intelligence search, caption translation, model-based
  Enhance Speech, Auto Reframe beyond heuristics, auto colour, Auto-Tag Audio Types.
- **Have:** local Whisper transcription (optional by default; shipped binary availability unverified, G19), Kokoro-82M text-to-speech
  narration (#278), scene edit detection, colour match.
- **Needs:** openly licensed models we can ship. **150–300 h.**

## G13. Delivery formats

- **Missing:** AV1 export (software or hardware), MPEG-2 / DVD / Blu-ray, DCP, JPEG 2000 MXF,
  AS-10 / AS-11, XDCAM HD MXF, P2, PIZ / PXR24 / B44 / DWA OpenEXR compression, MP3 audio,
  interlaced encoding, smart render, HEVC software encoder (HEVC exists only where a hardware
  encoder does).
- **Source check 2026-10-21:** AV1 has export-format and mux plumbing, but no built-in encoder
  or platform AV1 availability probe was found. It remains a backend gap, not an absent format id.
  FLAC audio export is implemented (`crates/flac`, `crates/export/src/lib.rs`).
- **Have:** PNG, TIFF, BMP, JPEG, Targa and DPX (10-bit) image sequences (#779); OpenEXR
  sequences (half float, linear; uncompressed, RLE, ZIPS or ZIP; #806, #874); AAC Audio
  (audio-only `.m4a`, or ADTS `.aac`; #814); ProRes 4444 / 4444 XQ in the export UI (#342).
- **Estimate:** 60–110 h. Doc: [file-format-parity.md](file-format-parity.md).

## G14. Localization: 3 of the 12 key languages missing (Hindi, Arabic, Vietnamese)

- **Have (recounted 2026-10-21):** nine non-English catalogs cover 98.9–100.0% of the current
  3,597-key catalog union, including Korean. Russian and Ukrainian each have 355 keys (9.9%).
  Counts measure catalog presence, not translation quality or all UI text. Full counts and method:
  [progress review](progress-review.md#recounted-localization-evidence).
- **Missing:** Hindi, Arabic (no RTL interface layout or catalog; the interface has Arabic and Hebrew
  fallback fonts and shows captions in reading order, but other names and fields keep egui's
  left-to-right run order; Arabic titles reported broken, #395),
  Vietnamese; Russian is partial. Korean is implemented, with a Hangul-font requirement. Engine errors,
  CLI and MCP stay English. Apart from the German menus (#630), no language has a recorded
  native-speaker review.
- **Implemented:** Indonesian (#867): `id.tsv` has 3,559 keys (98.9% of the current union), in Edit ▸
  Language, Settings ▸ General and the native macOS menu; System Language picks it for `id-*` (and
  legacy `in-*`) locales. Native-speaker review welcome.
- **Implemented:** Italian: `it.tsv` has 3,593 keys (99.9% of the current union), in Edit ▸ Language,
  Settings ▸ General and the native macOS menu; System Language picks it for `it-*` locales.
  Native-speaker review welcome.
- **Implemented:** German: `de.tsv` has 3,590 keys (99.8% of the current union), beyond the menus of
  #630 (panels, dialogs, effects and their parameters, settings, status messages). The menus are
  native-reviewed; review of the rest welcome.
- **Estimate:** 150–230 h. Doc: [localization-parity.md](localization-parity.md).

## G15. Interchange never validated in Avid / Pro Tools; slow XML import

- **Missing:** AAF and OMF exports have never been opened in Media Composer or Pro Tools; FCP7 XML
  / OTIO import of a Premiere sequence took 97 s / 66 s for three media files (#461); `file.import`
  merges into existing project content in headless round trips (#592).
- **Estimate:** 20–40 h (needs a human with Avid / Pro Tools, both installed on the owner's Mac).

## G16. Performance reports on 0.5.0

- **Missing:** steady UI under load. Constant lag after 0.4 → 0.5 (#525), ~100% CPU (#523). AV1
  software decode of a single-tile stream is bound by its serial tile decode (65–80 % of a
  frame; no row-level frame threading, references are waited for as whole frames); 8K is not
  real time.
- **Done:** Render (Effects) In to Out renders only the frames between In and Out, as partial
  previews named by segment hash and relative frames, instead of every segment In/Out touches
  (#424); Render Audio still renders whole audio segments. AV1 software decode runs deblocking,
  CDEF and loop restoration in parallel 64-row bands, bit-exact (#847; 1080p GOP 1.16–1.30×,
  low delay 1.28×, 2160p 1.14× at 16 threads). VP9 software decode loop-filters a frame as a
  wavefront over superblock rows on the decoder's pool instead of on one thread, bit-identical,
  1.2–1.5× faster with 16 threads (#980).
- **Estimate:** 30–60 h. Doc: [performance.md](performance.md).

## G17. Audio device and playback reliability

- **Evidence:** Asus Xonar DX output broken (#605), no audio (#602), audio cuts out with
  overlapping clips (#336), buggy audio (#410), Linux stutter (#617). No ASIO; cpal only.
- **Have:** a stream the device drops while playing (an ALSA xrun cpal reports as `POLLERR`, an
  unplugged device) is restarted with the same rate, channels, clock and mixer position, so sound
  resumes instead of playback going silent (at most 3 restarts in 10 s); a panicking mixer plays
  silence (#840).
- **Estimate:** 20–40 h.

## G18. Effects depth: approximations and missing behaviour

- **Missing:** Warp Stabilizer is 2-D (Premiere's is 3-D subspace), Morph Cut has no face tracking
  (and occlusion edges are soft), Lumetri HDR-aware maths,
  redesigned 26.0 mask tools (a stored, re-editable
  corner radius: rounding bakes fillets into the path, so a rounded corner cannot be re-rounded later).
- **Have:** Optical Flow time interpolation synthesises motion-compensated in-between frames (dense
  coarse-to-fine Lucas–Kanade, CPU; #780); Morph Cut morphs the subject along that optical flow
  instead of cross-dissolving (CPU; #805); Transform shutter angle motion blur (#415); Ultra Key
  Setting applied as a parameter preset (#458); rectangle and polygon masks round their corners
  with circular fillets (`masks.add` `cornerRadius`, `masks.roundCorners`, every path keyframe;
  #813), from an on-monitor corner-roundness handle with a live preview (one undo step per drag;
  #844); Shift snaps pen mask segments and dragged-out Bézier handles to 45° steps (#853), and
  Shift constrains vertex and whole-mask drags to 45° steps from the press point (#1408).
- **Source checked 2026-10-21:** multicam paging beyond sixteen angles and Edit Cameras are
  implemented and covered by `multicam_view_pages_beyond_sixteen_angles_and_edit_cameras` in
  `crates/ui-egui/tests/multicam_ui.rs` (test inspected, not run in this review).
- **Estimate:** 60–110 h.

## G19. Speech to text off in release builds

- **Confirmed in source 2026-10-21:** desktop and CLI manifests have empty default features and
  expose optional `whisper` support, including engine model downloads.
- **Unverified:** `.github/workflows/release.yml` delegates compilation to a shared workflow,
  which invokes platform packaging scripts. The [workflow follow-up](progress-review.md#github-workflow-follow-up)
  does not verify the feature flags through every packaging path or the shipped binaries. The
  earlier blanket statement that transcription is off in releases is not established by local
  manifests alone.
- **Next check:** inspect the shared release build and test a shipped binary with model setup,
  Transcribe Sequence and auto captions. Historical estimate: 5–10 h; not recalculated.

## G20. The web app shell

- **Implemented, source checked 2026-10-21:** browser file access, WebCodecs decode, WebAudio
  output, OPFS recovery and cooperative export. `apps/filmcraft-web/tests/smoke.mjs` covers load,
  playback, import and H.264 download. See [web.md](web.md).
- **Remaining / unverified:** run that smoke script; validate recovery and image-sequence downloads
  (historical Chrome report #378). The build is single-threaded; commands requiring worker threads
  remain limited. No browser session was executed in this review.
- **Historical estimate:** 40–80 h, not recalculated.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-21 | minor | Source audit of all twenty gaps linked; correct UI/docking/multicam/web and Korean status; qualify shared CI/release claims, historical issue reports and estimates |
| 2026-10-10 | major | Created from ROADMAP.md's "Where we are lacking" (2026-10-05); re-ranked against Premiere 26.5.2 and the 188 open issues; added `.prproj`, 10-bit / 4:2:2 camera decode, launch crashes and missing PR CI, localization, delivery formats |
| 2026-10-12 | minor | G10: MP1 (`A_MPEG/L1`) in Matroska imports (#1402) |
| 2026-10-12 | minor | G10: OpenEXR PXR24 import |
