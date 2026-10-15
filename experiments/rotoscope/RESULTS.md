# Local results — 2026-10-13–14

The actual EdgeTAM and ViTMatte checkpoints were exercised on an Apple M1 Pro
with 32 GiB unified memory. The initial runs below used Metal; later repeat-inference
checks exposed corrupted tracking output. The Python reference worker subsequently used
CPU tracking and validated Metal refinement. The current editor uses Rust CPU inference
(see the October 14 measurements below).
The local UI is available at http://127.0.0.1:7860 and the one-command launcher is
`./experiments/rotoscope/run.sh` from the repository root.

## Measured runs

Source: Amada44's CC BY-SA 3.0 sleeping-dog video, 1280 × 720, 25 fps. Output is
scaled, not a 4K source. Measurements are individual local runs, not a benchmark suite.
Model setup, frame loading, prompts, final alpha/cutout PNG writes and preview encoding
are excluded from the per-frame columns. Tracking includes mask readback and synchronization;
refinement includes trimap construction/PNG writes and alpha readback. Frame zero can
reuse a prompted mask.

| Device / mode | Frames | Output size | Mean tracking | Mean refinement | Evidence under `target/rotoscope/results/` |
|---|---:|---|---:|---:|---|
| Metal / Fast, browser | 30 | 720 × 404 | 149.87 ms | 0.05 ms (binary mask only) | `e548d283472039c2e46c9fed/benchmark.json` |
| Metal / Quality, browser + brush | 30 | 720 × 404 | 128.84 ms | 395.18 ms | `5f2823fde81a4f88c2215fa2/benchmark.json` |
| Metal / Quality, final browser run | 30 | 720 × 404 | 146.82 ms | 391.77 ms | `4595c3536924f18350107e5d/benchmark.json` |
| Metal / Quality, seed frame 6 | 12 | 720 × 404 | 141.48 ms | 505.52 ms | `4e0f60ba70ca218d615440f1/benchmark.json` |
| CPU / Fast | 3 | 512 × 288 | 480.71 ms | binary mask only | `06a331193ac4892dedf47581/benchmark.json` |
| CPU / Quality | 3 | 512 × 288 | 572.67 ms | 445.71 ms | `dfc3632a2207a8cc18a94682/benchmark.json` |

The different run sizes, source scale, warmup and devices mean these are not direct
CPU/GPU parity or speedup comparisons. The 30-frame Quality run used one foreground
click plus an 11,495-pixel brush on frame zero, with an 8-pixel automatic trimap band.
Its first five frames contained 46,687 / 40,227 / 39,067 / 39,680 / 39,653 soft alpha
pixels. This proves continuous alpha inference, not superior accuracy or hair quality.
The 30-frame brush run's largest sampled MPS driver allocation was about 1.20 GiB; transient peaks were
not measured. No ground-truth masks were supplied.

## Verified behavior

- **33 Python tests passed**, including definite-alpha locks, padded tile coverage,
  brush uncertainty, invalid model output, lossless alpha round trips, hostile prompts,
  unreadable media, missing weights and incomplete-cache cleanup after cancellation.
- Ruff lint and format checks passed. The shell launcher was exercised with the pinned
  dependencies and served the local UI.
- The Codex in-app browser exercised Load clip → foreground click → Fast tracking →
  Refine Edge brush → save brush → Quality tracking. Saved previews show the dog
  separated from the floor; no claim of ground-truth accuracy is made.
- The mid-frame CLI run produced all twelve masks: forward from frame 6 through 11,
  and backward through frame 0. Repeating it returned `cache_hit: true`.
- CPU fallback ran the actual segmentation model and the actual refinement model.
- Actual alpha PNGs were checked for finite [0,1] values and exact 0/1 values in
  definite trimap regions. The brush expands uncertainty on its saved frame only.
- A live browser job was cancelled; its incomplete directory was discarded.
- Cached alpha changed immediately when frame 12 was entered in the frame control.
  The generated preview played in the browser, with advancing progress and a completed
  duration. The final UI was left open with a Quality result.

## Initial native editor integration through the Python worker

The opt-in `experimental-ai-rotoscope` build now exposes the workflow under Effect
Controls → Opacity → AI Rotoscope. The Rust editor decodes the footage and sends bounded
PPM frames to a separate local model worker. Rendering and saved masks need no worker.

In the initial Metal run, the native 30-frame Fast job completed in 24.79 seconds, including cold
setup and Rust frame preparation. Quality with a ten-point Refine Edge stroke completed
in 67.88 seconds while Rust checks were also compiling. These are whole-job timings,
not inference-only speed comparisons. Both cached 720 × 405 alpha at exact 25 fps source
ticks. Fast contained binary alpha; the first five Quality frames contained 39,638 /
36,904 / 37,801 / 36,784 / 37,349 soft pixels. Accuracy remains unmeasured.

Include/Exclude clicks now request a one-frame preview. Included areas receive a
translucent green overlay. Excluded areas receive red only while Exclude is selected;
Include leaves the original background untinted. Switching tools changes the displayed
frame cache key and reuses the same alpha without another model job. The native UI
verified Include → Exclude → Include with unchanged points, jobs and project; a sampled
background pixel changed from RGB (222, 209, 193) to (237, 128, 120) in Exclude and
returned to its untinted color in Include. It is a
temporary Program-monitor project and never touches undo, project revision or saved
alpha. A warm new Fast preview completed in 0.78 seconds; initial model setup took
6.42 seconds. A cold Quality preview took 7.93 seconds; repeating it from cache took
0.19 seconds. Model timing varies with the machine and other work.

A later repeated-run check found corrupted Metal logits, even for repeated Fast-only
runs. Loading matting on CPU and explicit Metal synchronization did not resolve it.
At that stage, the default used CPU on this Mac (CUDA when available elsewhere); four repeated
Fast/Quality preparations on CPU produced identical coarse-mask hashes. Cache version
3 includes the actual backend, so results from the unstable runtime are not reused.
The new native 30-frame CPU Fast job completed in 27.51 seconds and Quality in 65.48
seconds. All 30 Quality frames preserved the matching Fast mask's definite foreground
and background. A cold CPU Quality preview completed in 4.72 seconds. Metal acceleration
is an open runtime issue, not a passed repeat-inference check.

Native controls were exercised through FilmCraft's UI/control channel: object selection,
Fast, Quality, Generate, Refine painting and click preview. Option-click removed precisely
one overlapping point (10 to 9), Cmd-click removed another (9 to 8), and Option-click away
from dots temporarily added an Exclude point while Include remained selected. Cmd-click
away from dots changed nothing. The original session points were restored after testing. The saved Quality project
was reopened with the model worker stopped, displayed frame 14 correctly and played
through to frame 29. Playback dropped frames under the concurrent compiler load; this
does not prove real-time throughput. Selection draft points are session-only.

Evidence remains local under `target/rotoscope/`: `FilmCraft-AI-Fast.fcproj`,
`FilmCraft-AI-Quality.fcproj`, `FilmCraft-AI-Fast-CPU.fcproj`,
`FilmCraft-AI-Quality-CPU.fcproj`, `FilmCraft-AI-Test.fcproj`, `editor-verification.json`
and `editor-final-overlay.png`. The launcher reopens the test project if it exists.

## Preview and mode-switch optimization

The worker now reuses encoded features when adding/removing points on the same CPU
preview frame and reuses complete tracked masks across Fast/Quality and edge-band/brush
changes. It skips repeated model-file setup after loading, preloads both models while
the editor opens, and keeps bounded intermediate caches. Native one-frame requests
poll every 20 ms instead of 150 ms. Cancellation resets shared state before releasing
the inference lock; partial tracked masks never enter the cache.

These CPU measurements use the same native-decoded 720 × 405 dog frames and prompts
before and after the change. New-point measurements exclude completed alpha-cache hits.

| Measurement boundary | Before | After | Verification |
|---|---:|---:|---|
| Native Fast preview, subsequent changed points on the same frame | 0.618 s median | 0.160 s mean | All three alpha results match byte for byte |
| Worker Quality preview, median of the last three changed-point runs | 1.316 s | 0.688 s | All five alpha results match byte for byte |

The first new-frame native Fast request took 0.594 s after the change; the next two
took 0.161 and 0.160 s. Feature reuse improves later clicks, not initial image encoding.
The worker Quality pass after a 30-frame Fast run took 19.11 s with a tracking-cache
hit. All 30 Fast and all 30 Quality alpha frames matched their baseline exactly.
For context, the previous native Quality job took 41.32 s, including editor frame
preparation and IPC; that boundary differs from the 19.11 s worker-only timing.
Cold full-clip Fast tracking did not improve consistently (19.66 s native baseline
versus 24.08 s worker after), so there is no claim of faster uncached full-clip tracking.

Evidence under `target/rotoscope/`: `native-previews-before.json`,
`native-previews-after.json`, `profile-before.json`, `profile-after.json`,
`full-after-benchmark.json`, `Perf-Before-Fast.fcproj`, `Perf-Before-Quality.fcproj`,
and preserved baseline alpha in `perf-held-cache/`. Six new cache tests cover memory
bounds, changed points/source pixels, setting and timestamp changes, and cancellation.
The editor was rebuilt and reopened with the original selection draft restored.

## Full Quality generation — October 14

Profiling the same native-decoded 30-frame clip found 107 padded ViTMatte tile predictions.
The Python reference worker was updated to keep EdgeTAM on CPU and uses Metal only for ViTMatte refinement.
It also caches independently completed 16-bit alpha frames in a 120-entry / 80 MB LRU.
Source pixels, trimap and actual refinement backend identify each frame. A brush change
on one frame preserves the other frames' alpha. New frame entries publish only after
the full job completes; cancellation publishes none. Automatic GPU failures restart
the whole refinement pass on CPU, with a fresh cache identity and no mixed-device save.

| Worker measurement, 720 × 405 / 30 frames | Before | After |
|---|---:|---:|
| Uncached full Quality job, models already loaded | 38.98 s | 27.97 s |
| Refinement portion of that job | 21.43 s | 11.22 s |
| Quality regeneration after a brush change on frame 14 | All frames previously refined again | 1.08 s; 29 alpha frames reused |

The full-job improvement is about 28%. These are matching worker boundaries, including
frame input processing and result encoding, with completed disk-cache hits excluded.
Model loading is excluded. CPU tracking remained about 17 s; this change accelerates
Quality refinement and later corrections, rather than claiming faster cold Fast tracking.
CPU thread candidates (4, 6 and 8) did not improve tracking; the existing four-thread
limit remains. Changing tile context or reducing output resolution was unnecessary.

All 30 Metal alpha frames were compared with the CPU baseline: the maximum difference
was one 16-bit alpha unit, affecting 0.107% of pixels. Definite regions remain exactly
0/1. Two subsequent full refinement passes, with frame-alpha reuse deliberately disabled,
produced identical Metal results and took 8.91 / 10.02 s using cached tracking. A changed
brush reused the other 29 frames exactly. This establishes repeatability and numerical
agreement on this sample, not ground-truth accuracy or performance on other hardware.

Seven new tests cover independent device selection, automatic CPU fallback, restarting
without mixed-device alpha, frame-specific reuse, source/band/backend invalidation,
timestamp changes and cancellation. All 33 Python tests and Ruff lint/format checks pass.
Evidence under `target/rotoscope/`: `clip-speed-before.json`, `clip-speed-after.json`,
their profiles and masks, `refinement-candidates.json`, `tracking-threads.json`, and
`hybrid-repeat-verification.json`. The standalone browser/CLI keeps its previous backend
policy; the hybrid default is limited to the native worker.

The actual native editor subsequently generated all 30 Quality frames in 35.91 s,
including Rust frame preparation, IPC and applying the saved mask. A brush correction
on one frame regenerated in 1.74 s. Those end-to-end timings have a different boundary
from the worker-only table above. Both native results were saved as
`Perf-Quality-Hybrid.fcproj` and `Perf-Quality-Hybrid-Correction.fcproj` for inspection.
The original test project remained byte-identical after undoing the verification masks,
and the original selection draft and green-only preview were restored.

## Rust inference and Refine Edge — October 14

The editor now defaults to in-process RTen 0.27.0 inference, using six checksum-pinned,
reproducible ONNX exports of the original checkpoints. Python is an offline exporter and
reference oracle. The launcher starts no inference server, and the Python worker was
stopped before native acceptance. Rust performs image preparation, prompt grouping,
bidirectional propagation, temporal memory, trimaps, tiled matting, 16-bit alpha and RLE.
The model package exported twice identically; the loader pins graph hashes independently
of its manifest, with bounded reads and unwind guards.

The port uses the CPU. It does **not** retain Metal acceleration. A 30-frame Quality job
in the actual editor completed in **131.77 s**, including native decoding/preparation,
inference and applying the validated mask; the preceding Python/Metal editor path took
35.91 s. These are local sample observations, not a claim that Rust is faster. Model
kernels/preprocessing are optimized in debug builds; four model threads leave cores
available to the editor. GPU inference remains necessary to recover the earlier speed.

Conformance uses original PyTorch weights with the same raw RGBA/bilinear normalization,
without the former JPEG convenience adapter. The three-frame Fast comparison matched
all pixels on frames 0/2; frame 1 differed by three threshold pixels (IoU 0.999963).
Quality matched within one 16-bit step on frames 0/2; the differing threshold pixels on
frame 1 changed its trimap, with mean absolute alpha difference 0.709/65535 and maximum
4492/65535. A second case with multiple Include/Exclude points, a nonzero seed, backward
tracking, rectangular tiles and a brush matched within **one 16-bit alpha step** on all
three frames. These are numerical checks, not hair/occlusion ground-truth accuracy.

Refine Edge now automatically uses Quality and invokes ViTMatte after a completed stroke.
Paint defines unknown mixed foreground/background pixels, rather than setting opacity.
The edge-region view displays inferred grayscale alpha locally over the footage; Alpha
shows the full matte and Composite applies the actual cutout. View changes reuse alpha.
Generated frames can be refined without fresh Include clicks; existing soft alpha outside
the brush is preserved exactly. Strokes are currently frame-local. This follows the
functional distinction documented in [Adobe's Refine Edge workflow](https://helpx.adobe.com/after-effects/using/roto-brush-refine-matte.html);
no Adobe artwork, media or implementation is included.

Cmd/Ctrl-Z and redo now include additions, deletions and Clear points in chronological
history with document edits. A brush gesture is one undo step; transient draft edits do
not dirty the project. The actual editor verified clear/redo, Option-delete/undo and
brush-add/undo. The latest live project and point were preserved through the rebuild.
After the Mac was unlocked, native acceptance also verified Edge region, Alpha and
Composite on screen. A dragged stroke added 20 samples, ran Quality automatically in
2.81 s and was removed in one Cmd-Z; the project remained unchanged. That timing reused
the frame's tracked mask and measures a local correction, not full-clip inference.

A real-model cache check changed only frame 1 of three; frames 0/2 were bit-identical.
Refining a previously generated frame changed 791 painted pixels to fractional alpha and
preserved every pixel outside the brush exactly. Saved evidence is under
`target/rotoscope`: `native-cache-verification.json`, `native-rust-ui-verification.json`,
`native-rust-multiple-quality.json`, `native-python-multiple-quality.json`,
`Native-Rust-Quality.fcproj`, `refine-native-acceptance.json`, `refine-real-painted-region.png`,
`refine-real-painted-alpha.png`, `refine-real-composite.png` and the model manifests. Model-only sequence timings were
12.95 s initially / 6.31 s for the changed-frame correction / 2.63 s for a local base-matte
correction; background builds/editor work were active, so these are conformance timings.

The new engine suite passed **648 tests**, with 3 intentionally ignored; UI unit tests
passed **192**, and native inference helper/package tests passed **7**. Strict engine and
inference Clippy passed. UI Clippy retains the same existing `nonminimal_bool` and
`collapsible_match` baseline findings; it passes with only those classes excepted.

## Native checks and remaining work

Rust 1.95 was installed side by side; the default toolchain was preserved. Ten AI
engine tests passed, including single-point removal and preview refresh, invalid point
indices, preview alpha preservation, no preview undo/save edits,
atomic mask undo/redo and persistence, invalid inputs, cancellation, and rejecting a
clip edited during inference. Seven mask-render tests passed. The 33 reference Python
tests, Ruff lint/format and shell syntax checks passed. Dependency layers, attribution
checks (69 assets) and the WebAssembly check (49 crates) passed.

The engine, project, format, render and GPU suites completed with **981 passed,
5 ignored, zero failures**, including source-frame raster playback and CPU/GPU
compositing of uploaded cached alpha. Strict Clippy on those five affected crates passed.

Strict workspace Clippy found existing `nonminimal_bool` / `collapsible_match` warnings
in `panels/file_dialogs.rs:134` and `panels/timeline.rs:268,2531`. With only those two
baseline lint classes excepted, the workspace check passed. The full workspace test
attempts were interrupted to prioritize the requested native preview updates; no claim
of a completed `cargo xtask ci` run is made.

This is a local native prototype, disabled in standard builds. Rust GPU inference,
temporal alpha regularization, optical-flow brush propagation, a dedicated GPU alpha
shader, NVIDIA performance, native 1080p/4K inference, and occlusion/thin-structure
accuracy remain open. Masked layers use existing CPU preparation before GPU composition.
New saves use schema 16, which older editor builds refuse rather than lose the masks.

## Inspect the actual outputs

Each completed result directory contains `benchmark.json`, `overlay.mp4`,
`preview.mp4`, `alpha/` (16-bit grayscale), `rgba/` (8-bit transparent PNGs),
`trimaps/` (Quality only), `masks/`, attribution sidecars and `alpha-and-cutouts.zip`.
All runtime media and weights remain in the gitignored `target/rotoscope/` directory.
The source and model revisions and SHA-256 values are recorded in each benchmark.

## Draft PR verification and demo — October 15

The demo is a 62.95-second, 1920×1080 H.264 capture of the actual native editor.
It covers Include/Exclude previews, Option-click removal, undo of removed/cleared
points, Quality/Refine Edge, Alpha/Composite, generation and cached playback. The
recorded 30-frame Quality generation took 118.96 seconds; its wait is trimmed and
labeled. The movie and its CC BY-SA 3.0 sample attribution remain under ignored
`target/rotoscope/`; the movie is intended as a PR attachment, not a repository asset.

At the contributor's request, compilation and verification moved to Google Colab
through its CLI. The Linux x86_64 runner has eight logical CPUs (Intel Xeon
Broadwell) and about 51 GiB of RAM. Rust 1.95 release checks used
`CARGO_PROFILE_RELEASE_LTO=false`, `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16` and
`CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C target-cpu=native"` to reduce
test compilation costs on that runner. Shipping profile settings are unchanged.
The source was transferred as a bounded patch and its remote staged Git tree was
checked against the local tree. No inference weights or video were transferred
for these unit checks.

After rebasing onto upstream 0.8.0, the complete `cargo xtask ci` gate passed:
format, strict workspace Clippy, **3,518 tests passed / zero failed / 44 intentionally ignored**, dependency
layers, 69 asset attributions and WebAssembly (50 crates). Additional strict
workspace Clippy with `filmcraft/experimental-ai-rotoscope` enabled passed.
Before the upstream rebase, the additional feature-enabled engine suite passed
**649 tests**, with three intentionally ignored, including all 15 AI-mask
regressions; all seven native inference package unit tests passed. The inference
implementation was unchanged by the rebase. The full workspace gate and
inference-enabled strict Clippy were repeated on the rebased code. These completed results supersede the earlier partial CI
observations above; no lint exceptions, lowered thresholds or new test skips were
used.

The external test oracle was FFmpeg/FFprobe
`n8.1.3-18-g46d899d7a0-20261015`, rather than Colab's older 6.1.1. With the newer
oracle, unchanged phone H.264 workflow, APV export probe, Matroska duration and
EXR/TIFF comparison tests passed. FFmpeg remains an external test tool and is
neither linked nor shipped.

The original portable shipping profile (thin LTO, one codegen unit) separately
passed the existing 24-track mixer gate at **4.2x realtime** on this Colab runner;
the CI profile passed at **4.4x**, both above the unchanged 4x threshold. All 24
mixer correctness tests passed, including sample-exact request partitioning,
automation after the last keyframe and live overrides. The retained optimization
reuses an animated lane's final value after its last keyframe, avoiding redundant
per-sample time, automation and gain conversion. These audio measurements do not
measure rotoscope inference speed.

PR preparation also fixes four existing strict-lint expressions, restores the
Linux dev desktop entry's packaged MIME list and adds a regression for individual
linked selections. The rebased branch retains upstream's linked-selection and
Spanish-notice fixes, and the new crate's lockfile version matches 0.8.0.
The AI controls are hidden when no inference backend is configured, preserving the
standard editor's mask/keyframe layout. Their existing UI regressions passed in
the complete run. These changes add no model runtime to standard builds.
