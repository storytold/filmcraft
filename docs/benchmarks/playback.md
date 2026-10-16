# Playback recording benchmark: fork vs upstream (web build)

Plays a 960×540 clip in the web app and reads the app's own `perf.stats` counters: frames shown vs dropped by
the playback clock, and software H.264 decode time per sample. Complements `jank.md` (1080p, rAF pacing).

- **upstream**: `storytold/filmcraft` @ `cee1feb` (fork point). **fork**: see each run below.
- Builds: `cargo xtask web` (release, wasm-bindgen 0.2.129, no wasm-opt), the same `dist/` dirs `jank/run.sh` builds.
- Clip: 960×540, 30 fps, 40 s, 1200 frames, H.264, copied into each `dist/` as `clip.mp4`.
- URL `/?nowebcodecs` (forces the wasm software decoder), WebGL via ANGLE/SwiftShader, headless Chromium 1243,
  1280×800 viewport, Playwright video recording on.
- Script: `scripts/playback/rec.mjs <url> <label> <outdir>`: wait for load, `importUrl('clip.mp4')`,
  `file.newSequence`, settle 3 s, `ui.playback play`, record rAF intervals for 30 s (`DUR`), then dump `perf.stats`.
  `rec2.mjs` is the same plus `CPU=<n>` → CDP `Emulation.setCPUThrottlingRate`. `coi.py <port> <dir>` serves
  with COOP/COEP so the threaded build gets `crossOriginIsolated`.
- Raw JSON: `scripts/playback/results/` (`r-*` = old run, `n-*` sequential, `p-*` concurrent, `t4-*` 4× throttle, `n-mt-*` threaded).

## Old run (2026-10-13 ~20:08 PT, fork `4d2efb4`, adaptive budget phase 1c)

Fork and upstream runs overlapped in time (file times 20:08–20:11). Box load was not recorded.

| run | rAF avg ms | shown | dropped | drop rate | decode ms/sample |
|---|---|---|---|---|---|
| fork 1 / 2 / 3 | 65.4 / 60.5 / 57.0 | 19 / 20 / 36 | 823 / 824 / 805 | 97.7 / 97.6 / 95.7 % | 5.99 / 5.60 / 5.36 |
| upstream 1 / 2 / 3 | 70.5 / 59.8 / 59.0 | 112 / 411 / 472 | 730 / 432 / 375 | 86.7 / 51.2 / 44.3 % | 7.05 / 6.17 / 6.00 |

At the time we suspected the adaptive pump budget (`crates/ui-egui/src/lib.rs`, `ui()`, ~line 2116) being stuck at its 4 ms floor.

## New run (2026-10-13 21:29–21:47 PT, fork `2086529`)

8 vCPU box. Load average (1 min) shown per block.

| set | build | rAF avg ms | p95 ms | shown | dropped | decode ms/sample | load |
|---|---|---|---|---|---|---|---|
| sequential ×3 | fork | 16.7 / 16.7 / 16.9 | 16.8 | 901 / 901 / 900 | 0 / 0 / 1 | 3.94 / 3.80 / 3.98 | 1.8→4.2 |
| sequential ×3 | upstream | 16.8 / 16.9 / 16.8 | 16.8 | 901 / 901 / 899 | 0 / 0 / 0 | 4.76 / 5.02 / 4.86 | 1.8→4.2 |
| concurrent ×3 (fork+up at once, like the old run) | fork | 17.1 / 17.2 / 17.0 | 16.8 | 899 / 901 / 899 | 1 / 0 / 0 | 4.89 / 4.79 / 4.78 | 3.3→9.5 |
| concurrent ×3 | upstream | 17.2 / 17.4 / 17.2 | 16.8 | 897 / 898 / 900 | 3 / 3 / 0 | 5.88 / 5.94 / 5.82 | 3.3→9.5 |
| 4× CPU throttle ×2 | fork | 96.6 / 99.1 | 117–133 | 4 / 5 | 908 / 911 | 14.7 / 15.2 | 8.7→2.1 |
| 4× CPU throttle ×2 | upstream | 125.8 / 112.8 | 150–233 | 5 / 5 | 898 / 909 | 19.0 / 17.8 | 8.7→2.1 |
| threaded (`--threads`, COOP/COEP) ×2 | fork-mt | 16.8 / 16.8 | 16.7 | 897 / 900 | 3 / 0 | 4.22 / 4.15 | 2.7→5.0 |

The threaded build only built with a local, uncommitted xtask patch (see "Threaded build" below).

## Findings

- **The fork-only frame drop did not reproduce.** At current fork main, fork and upstream both show ~900/900 frames at
  a steady 60 Hz rAF, run one after the other or at the same time (load up to 9.5). With 4× throttling both fall over
  the same way (≥99 % dropped), so this is not a fork-only problem. The pump-budget code has **not** changed since the
  old run (`git diff 53d7248..2086529` touches lib.rs only for the threaded repaint flag). So nothing was "fixed".
  The old run more likely reflects that session's environment: rAF was ~16 fps for **both** builds then and is 60 fps now.
  We could not recreate those conditions, so we can't tell whether the budget floor makes things worse when rAF is
  slow. The throttled runs show no fork-specific penalty.
- **Decode:** fork is ~18 % faster per sample (3.9 vs 4.9 ms sequential, 4.8 vs 5.9 concurrent, 15 vs 18 ms at 4×).
  This matches the old run (5.4–6.0 vs 6.0–7.1 ms) and the SIMD conversion bench.
- **Threaded build:** loads with `threaded: true` and behaves like the single-threaded build on this 540p clip, with no
  measurable gain. The perf.stats `workers` field is 3 in every build, including single-threaded ones.

## Threaded build

`cargo xtask web --threads` at `2086529` (nightly rustc, rust-src, wasm-bindgen 0.2.129) fails in wasm-bindgen with
`failed to prepare module for threading: failed to find __wasm_init_tls`. The fix is to add
`-C link-arg=--export=__wasm_init_tls --export=__tls_size --export=__tls_align --export=__tls_base` to the threaded
RUSTFLAGS in `xtask/src/main.rs`. With that change the build succeeds and the measurements above were taken.
