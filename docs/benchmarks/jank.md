# Browser jank benchmark: fork vs upstream (web build)

Does playback of the web app stutter, and did the fork's web-perf work (SIMD YUV→RGBA, adaptive
frame-pump budget) change that? This bench plays the same 1080p clip in both web builds under
headless Chromium and records frame pacing.

- **fork**: `yishengjiang99/filmcraft` @ `53d7248` (measured build; the later `0a3a9da` threaded-wasm commit was not benchmarked)
- **upstream**: `storytold/filmcraft` @ `cee1feb` (fork point)
- Both built with `cargo xtask web` (release, wasm-bindgen 0.2.129, no wasm-opt on PATH for either), served as static files with `python3 -m http.server`.

## Rerun

```sh
docs/benchmarks/scripts/jank/run.sh                       # defaults: RUNS=3 SECONDS_=20 CPUS="1 4"
RUNS=5 SECONDS_=30 CPUS="1 2 4" docs/benchmarks/scripts/jank/run.sh
```

`run.sh` installs wasm-bindgen-cli/playwright-core if missing, checks out `cee1feb` as a git worktree,
builds both apps into separate target dirs (`WORK=/tmp/fc-jank`), generates the clip with ffmpeg,
serves the fork on :18731 and upstream on :18732 (`PORT_FORK`/`PORT_UP`), runs `bench.mjs` for each,
and writes `results/*.json` + `results/table.md`. Playwright's Chromium must be installed
(`npx playwright-core install chromium-headless-shell`).

## Method (`scripts/jank/bench.mjs`)

1. Fresh browser context per run, 1600×1000 viewport, DPR 1, headless Chromium (Playwright 1.63, Chromium 153).
2. The frame recorder from `apps/filmcraft-web/tests/smoke.mjs` is injected before navigation in **both** builds
   (upstream has no recorder of its own; it is the same init script).
3. Wait for `filmcraftLoad.readyMs`, `filmcraft.importUrl("clip1080.mp4")`, `file.newSequence` from it, settle 3 s.
4. Optional `Emulation.setCPUThrottlingRate` (4×) via CDP.
5. Start recording, `ui.playback play`, wait 20 s, stop. Clip: 25 s, 1920×1080, 30 fps, H.264 8 Mb/s, `testsrc2` pattern.
6. Metrics: rAF intervals (p50/p95/p99/max, % > 16.7 ms, % > 33.4 ms — i.e. two 60 Hz vsyncs),
   `longtask` entries (>50 ms) observed during the window (count, total ms), and decoded fps = Δ`perf.stats`
   `playback.shown` / wall time (`dropped` is the matching Δ). Medians are across the runs.

**Recorder fix (bench copy only).** In smoke.mjs `__fcLast` starts at `performance.now()` at script time and keeps
ticking while not recording, so the first recorded delta is taken against whatever tick preceded `recording = true`
and the very first one mixes a `now()` with a rAF timestamp. The bench only records a delta when both the previous
and current tick were inside the recording window. Long tasks are also counted only inside the window
(smoke.mjs counts them for the whole session). App source is unchanged.

## Results (Oct 13 2026, 3 × 20 s per cell)

| build | CPU | rafFps | p50 | p95 | p99 | max | over16_7pct | over33pct | longtasks | longtaskMs | decodedFps | dropped |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| fork run1 | 1x | 30.1 | 33.3 | 66.7 | 116.6 | 166.6 | 80.1 | 25.7 | 69 | 4742 | 0.1 | 581 |
| fork run2 | 1x | 30.8 | 33.3 | 66.7 | 116.6 | 150.1 | 79.6 | 22 | 40 | 3120 | 0.3 | 573 |
| fork run3 | 1x | 31.1 | 33.3 | 66.7 | 116.6 | 150.1 | 78.2 | 21 | 45 | 3590 | 0.1 | 585 |
| **fork median** | 1x | **30.8** | **33.3** | **66.7** | **116.6** | **150.1** | **79.6** | **22** | **45** | **3590** | **0.1** | **581** |
| upstream run1 | 1x | 28.4 | 33.3 | 50 | 66.7 | 83.3 | 95.4 | 26.9 | 9 | 558 | 0.2 | 585 |
| upstream run2 | 1x | 29.5 | 33.3 | 50 | 50.1 | 66.7 | 93.4 | 24.2 | 8 | 454 | 0.5 | 584 |
| upstream run3 | 1x | 29.4 | 33.3 | 50 | 66.7 | 83.3 | 93.1 | 22.7 | 14 | 791 | 0.4 | 582 |
| **upstream median** | 1x | **29.4** | **33.3** | **50** | **66.7** | **83.3** | **93.4** | **24.2** | **9** | **558** | **0.4** | **584** |
| fork run1 | 4x | 30.7 | 33.3 | 50.1 | 133.3 | 233.4 | 80.7 | 26.8 | 11 | 1363 | 0.1 | 577 |
| fork run2 | 4x | 28.1 | 33.3 | 50.1 | 133.3 | 183.4 | 89 | 32 | 11 | 1250 | 0.2 | 575 |
| fork run3 | 4x | 27.7 | 33.3 | 50 | 150 | 216.7 | 92.1 | 28.8 | 12 | 1469 | 0.1 | 574 |
| **fork median** | 4x | **28.1** | **33.3** | **50.1** | **133.3** | **216.7** | **89** | **28.8** | **11** | **1363** | **0.1** | **575** |
| upstream run1 | 4x | 19 | 50 | 66.7 | 166.8 | 266.7 | 99.7 | 80.4 | 39 | 2915 | 0 | 580 |
| upstream run2 | 4x | 18.6 | 50 | 66.8 | 166.7 | 183.4 | 99.5 | 85.6 | 35 | 2796 | 0 | 581 |
| upstream run3 | 4x | 18.5 | 50 | 66.8 | 150 | 183.4 | 99.5 | 85.8 | 32 | 2549 | 0.1 | 576 |
| **upstream median** | 4x | **18.6** | **50** | **66.8** | **166.7** | **183.4** | **99.5** | **85.6** | **35** | **2796** | **0** | **580** |

## Reading it

- **Neither build actually plays the 1080p clip here.** Decoded fps is 0–0.5 against a 30 fps clip; almost every
  due frame is counted as dropped. The numbers above therefore measure the *UI loop's* pacing while the decoder is
  starved, not smooth video playback. Treat them as a jank probe of the app shell, not of video throughput.
- rAF runs at ~30 Hz in both (p50 33.3 ms), so headless Chromium on this VM isn't delivering 60 Hz; "% > 16.7 ms"
  is mostly that cap and is not a meaningful jank signal here. "% > 33 ms", p95/p99/max and long tasks are.
- **1× CPU:** fork has a slightly lower share of >33 ms frames (22 % vs 24 %) and higher rAF rate, but worse tails
  (p95 66.7 vs 50, p99 116.6 vs 66.7 ms) and many more long tasks (45 / 3.6 s vs 9 / 0.56 s). Mixed; the fork's
  extra main-thread work per pass shows up as long tasks.
- **4× CPU throttle:** fork holds ~28 rAF fps with 29 % > 33 ms; upstream falls to ~19 fps with 86 % > 33 ms and
  35 long tasks (2.8 s) vs fork's 11 (1.4 s). This is consistent with the fork's adaptive frame-pump budget
  shedding work when frames get expensive, though p99/max tails are still large in both.

## Caveats

- Headless Chromium in a Linux VM, **no GPU** (software rendering, no hardware video decode). Real browsers on real
  hardware will look very different; decoding in particular.
- **120 Hz was not tested.** CDP/Playwright has no display refresh-rate emulation and headless here vsyncs at ~30 Hz,
  so the 120 Hz branch of the adaptive budget is not exercised. CPU throttle 4× was used as the budget probe instead.
- 3 runs per cell on a shared 8-vCPU VM; run-to-run spread is visible in the table (e.g. fork 1× long-task total
  3.1–4.7 s). Fork and upstream were run back to back, not interleaved per run.
- The `0a3a9da` threaded build (`--threads`, needs cross-origin isolation) is not covered; the static server sends
  no COOP/COEP headers.

## Rerun 2026-10-13 21:21–21:29 PT: fork `2086529` vs upstream `cee1feb`

Same script and defaults (`run.sh`, RUNS=3, 20 s, CPU 1× and 4×). 8 vCPU box, 1-min load 0.1 at start and 2.1 at end.
`results/*.json` and `results/table.md` now hold this rerun; the `53d7248` numbers are in git history (`a10ebce`).

| build | CPU | rafFps | p95 | p99 | max | >16.7 % | >33 % | longtasks | longtask ms | dropped |
|---|---|---|---|---|---|---|---|---|---|---|
| fork 53d7248 (old) | 1× | 30.8 | 66.7 | 116.6 | 150.1 | 79.6 | 22 | 45 | 3590 | 581 |
| **fork 2086529** | 1× | 29.7 | 50.1 | 50.1 | 66.7 | 80.3 | 40.2 | 7 | 446 | 581 |
| upstream (old) | 1× | 29.4 | 50 | 66.7 | 83.3 | 93.4 | 24.2 | 9 | 558 | 584 |
| **upstream (new)** | 1× | 29.6 | 50 | 50.1 | 83.4 | 93.6 | 24.3 | 8 | 424 | 583 |
| fork 53d7248 (old) | 4× | 28.1 | 50.1 | 133.3 | 216.7 | 89 | 28.8 | 11 | 1363 | 575 |
| **fork 2086529** | 4× | 27.4 | 50.1 | 100 | 200.1 | 95.4 | 26.4 | 9 | 1256 | 578 |
| upstream (old) | 4× | 18.6 | 66.8 | 166.7 | 183.4 | 99.5 | 85.6 | 35 | 2796 | 580 |
| **upstream (new)** | 4× | 19.7 | 66.7 | 183.3 | 200 | 99.5 | 81.3 | 13 | 1564 | 577 |

At 4× the fork still has the clear lead (27 vs 20 rAF fps, 26 % vs 81 % of frames over 33 ms). At 1× the old fork
long-task spike (45 tasks, 3.6 s) is gone (7 tasks, 0.45 s), now in line with upstream. In this 1080p bench both builds
still drop ~97 % of video frames at 1080p (decode-bound on software decode).
