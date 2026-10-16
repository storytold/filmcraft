# Web optimization plan: eliminating jank in the browser build

Status: plan (2026-10-13). Scope: `apps/filmcraft-web` and the shared crates it runs
(`filmcraft-ui-egui`, `filmcraft-engine`, `filmcraft-gpu`, `filmcraft-render`,
`filmcraft-media`, the codec crates). Background: [web.md](web.md) describes the current
build; [performance.md](performance.md) holds the desktop measurements this plan reuses.

## The one-sentence diagnosis

The desktop app is a multithreaded program — a pool of 2–6 frame-worker threads
(`crates/ui-egui/src/frames.rs`, `available_parallelism().clamp(2, 6)`) decodes, converts
and composites off the UI thread — but the web build runs **single-threaded**: the wasm
module is built without atomics, so `std::thread` does not exist, `rayon` runs inline,
and every subsystem is time-sliced onto the UI thread. Jank is what that time-slicing
looks like from the user's side.

Concretely, in one 16.6 ms display frame the UI thread may have to do all of:

| Work | Where | Cost driver |
|---|---|---|
| egui input, layout, paint (whole app, one canvas) | `crates/ui-egui` | panel redraws, timeline, transcript layout |
| Frame jobs: decode + composite + texel conversion | `FrameServer::pump`, capped at **24 ms while playing, 40 ms idle** (`crates/ui-egui/src/lib.rs`) | software decode of 1080p H.264 is ~32 ms of CPU per frame on an M4 Pro (performance.md HW1); the pump cap alone exceeds the frame budget |
| Export steps | `Session::pump_jobs`, ~30 ms per UI frame | encoder + CPU compositor (~6.5–7.8 ms/frame at 1080p before encode) |
| Audio mixing | `apps/filmcraft-web/src/audio.rs`: mix 250 ms ahead in 2048-frame blocks, `postMessage` to the worklet | recurs every ~43 ms; copies every block |
| Blob fetch retries | `BlobReader` (`filmcraft-media`): 384 MiB LRU of 1 MiB chunks, `WouldBlock` + retry | a cache miss drops the frame's result and re-queues the job; seeks pay keyframe + fetch latency on the UI thread |
| WebCodecs colour conversion | `VideoFrame` → RGBA by drawing to a 2D `OffscreenCanvas` (`apps/filmcraft-web/src/webcodecs.rs`) | a GPU→CPU→GPU round trip per frame |
| Scopes | `filmcraft-scopes::compute` on the CPU | histogram / waveform / parade reductions over every pixel, per refresh |

Playback is audio-clocked and the worklet starves gracefully (a starved worklet holds
the playhead), so overload shows up as dropped video frames, a playhead that stalls,
sluggish input and scrubbing that lands late — not as crashes. Features that need a
thread on the desktop (render previews, proxies, Project Manager, mask tracking)
currently **return an error** on the web.

The good news: the deployment preconditions for fixing this are already half in place.
`cargo xtask web --serve` sends `Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp` (xtask/src/main.rs), and
`apps/filmcraft-web/src/lib.rs` already detects and reports `crossOriginIsolated` via
`filmcraft.info()`. What is missing is the threaded wasm build itself: the wasm-opt
invocation enables `bulk-memory` but not threads/atomics, and there is no `build-std`
step anywhere in the build.

## Technology evaluation

### 1. WebAssembly threads + SIMD (the foundation)

Two independent upgrades hide under "WebAssembly":

- **SIMD (stable, ship first).** `wasm32` v128 SIMD needs no nightly toolchain, no
  cross-origin isolation and no page headers. The hand-written codec hot loops
  (H.264/HEVC/VP9 IDCT and prediction, colour conversion, audio DSP) and the CPU
  compositor are the targets. The roadmap already flags the cost of its absence:
  AV1 decodes at ~4 fps at 1080p today, with "AV1 threading/SIMD" listed as the fix
  (docs/roadmap.md, M9). Add `--enable-simd` to the wasm-opt flags in xtask and
  `+simd128` to the web target features; gate kernels with
  `#[cfg(target_feature = "simd128")]` so non-SIMD fallbacks stay bit-exact (the codec
  parity tests are the safety net).
- **Threads (needs nightly `build-std` + atomics).** A `std` built with
  `-Z build-std=std,panic_abort` and `-C target-feature=+atomics,+bulk-memory,+mutable-globals`
  gives real `std::thread::spawn`, which wasm-bindgen maps onto Web Workers sharing one
  linear memory. This is the single change everything else in this plan hangs off: it
  lets the web build run the *same* FrameServer worker pool as the desktop instead of
  the pump. Keep the toolchain pinned and web-only (desktop stays on stable), the same
  way the build already pins the wasm-bindgen CLI version. Ship both artifacts — the
  loader picks the threaded build when `crossOriginIsolated` is true and falls back to
  today's single-threaded build otherwise, so non-isolated hosting keeps working.

Also set wasm memory to a fixed, preallocated size for the threaded build: memory
growth during playback pauses every thread on the shared memory and shows up as a
periodic hitch.

### 2. Worker threads (Web Workers)

Two flavours, both wanted:

- **The wasm thread pool (in-module workers).** Once §1 lands, `frames.rs` flips from
  `pump()` to its worker mode with no redesign — the abstraction already exists
  ("Without worker threads (wasm32): run queued jobs on the calling (UI) thread" is a
  fallback branch, not the design). Decode, texel conversion and GPU-plan building move
  off the UI thread; the 24/40 ms pump caps disappear. Size the pool like the desktop:
  `available_parallelism` clamped to 2–6, minus headroom for the audio and main
  threads (browsers report logical cores; oversubscribing the pool re-creates the jank
  as contention).
- **Dedicated task workers (separate modules).** Long, latency-tolerant jobs should
  not share the frame pool at all: media indexing (the Matroska cluster scan web.md
  calls out as slow), thumbnail and waveform generation, proxy and render-preview
  rendering (un-errors those features), project save/serialisation, the background
  OPFS media copies (up to 4 GiB today), speech/TTS jobs, and **export** — today the
  exporter advances one frame per 30 ms slice of UI-thread time, so an export taxes
  every interaction for its whole duration; in a worker it becomes a background job
  that posts progress. Each of these is a small wasm (or JS) module with a message
  API; none needs the frame pool's priority queue.

Audio mixing is a special case: today the UI thread mixes 250 ms ahead and posts
blocks. With threads, mixing belongs on one of the worker threads (it is exactly the
desktop's arrangement), feeding the worklet through §3's ring buffer.

### 3. SharedArrayBuffer

Cross-origin isolation unlocks `SharedArrayBuffer`, which is both the transport for
wasm threads (shared linear memory) and a zero-copy channel in its own right:

- **Audio ring buffer.** Replace per-block `postMessage` of `Float32Array`s (copied,
  and a GC source at ~23 blocks/second) with a lock-free ring in a `SharedArrayBuffer`
  that the `AudioWorkletProcessor` reads directly. The worklet already tracks
  `played` frames and reports its clock; with a shared ring it can also publish its
  read index into shared memory, so the mixer sees true latency without a message
  round trip and the audio clock stays master with tighter bounds.
- **Frame exchange.** Decoded planes already live in `filmcraft_frame::pool` on the
  desktop; with shared memory, a decode worker's output *is* the compositor's input —
  a slot index changes hands, not 8–33 MB of pixels. This is what makes §2's pool
  cheap at 1080p and viable at 4K.
- **Shared chunk cache.** `BlobReader`'s 384 MiB LRU is per-instance today. In shared
  memory (or a dedicated I/O worker owning fetches), decode workers stop duplicating
  fetches and the read-ahead (3 chunks) amortises across all consumers.

Isolation is a deployment contract, not just a build flag: COOP/COEP `require-corp`
must be sent by whatever hosts production, and it forbids embedding the app in
third-party pages without their own isolation. Evaluate `COEP: credentialless` as the
softer variant; keep the non-isolated single-threaded build as the permanent fallback
(§1). Safari supports SAB under isolation but has historically been the last to each
of these features — the fallback build is what Safari-without-isolation users get, so
it must not rot.

### 4. Atomics

Atomics are the synchronisation layer that makes §§2–3 safe without message passing:

- **In Rust:** with the atomics target feature, `core::sync::atomic` compiles to real
  shared-memory atomics, and `std`'s `Mutex`/`Condvar` park threads with
  `Atomics.wait` (the wasm futex). The FrameServer's priority queue becomes a blocking
  queue like the desktop's: idle workers sleep instead of the current web pattern of
  re-checking the queue every egui frame.
- **In JS:** `Atomics.load/store/add` on the audio ring's head/tail indices and on
  frame-slot state (empty → filling → ready), and `Atomics.notify` to wake a worker
  the moment a `BlobReader` chunk lands — replacing the drop-and-retry `WouldBlock`
  dance *inside workers* with wait-for-data.
- **Hard rule:** `Atomics.wait` throws on the main thread. The UI thread only ever
  polls (`Atomics.load`, try-lock); anything that can block lives in a worker. This
  asymmetry should be written into the worker APIs from the start, not discovered per
  call site. Perf counters (`perf.stats`) should move to plain atomic increments while
  this is being touched — they are read from both sides already.

### 5. WebGPU

WebGPU is already the primary path (eframe creates the device; `filmcraft-gpu`
composites on it), so this is about closing the three gaps where work falls back to
the CPU on the UI thread:

- **WebCodecs import without the 2D-canvas round trip.** Convert `VideoFrame`s with
  `copyExternalImageToTexture` / `importExternalTexture` straight into the
  compositor's texture set instead of drawing to an `OffscreenCanvas` 2D context and
  reading RGBA back. This is per-frame work at the heart of playback of the most
  common files (H.264/HEVC in MP4).
- **Compute scopes.** Histogram, waveform and RGB parade are per-pixel reductions —
  textbook compute shaders with atomic binning. `filmcraft-scopes` computing them on
  the CPU for every refresh is pure UI-thread tax during colour work; a compute path
  in `filmcraft-gpu` removes it and lets scopes refresh every frame instead of on a
  budget.
- **The WebGL2 fallback must stop meaning "CPU compositor on the UI thread."** When
  WebGPU is absent, frames composite on the CPU — today on the UI thread via the
  pump. After §§1–2, that work runs on pool workers and only the finished image
  crosses to the UI; the fallback gets slower frames, not a jankier UI. WebGPU is
  also available inside dedicated workers in Chromium, which makes worker-side
  preview/proxy rendering (§2) a GPU job rather than a CPU one; treat that as an
  optimisation of the worker path, not a dependency.
- The desktop numbers say why this ordering matters: moving effects to the GPU made
  effect-stack playback go from ~250 ms of worker time per frame to real-time, and
  GPU exports 2.4–3.2× faster (performance.md, GPU2/GPU3). On the web those same
  fallbacks are not just slower — they share a thread with input handling.

### 6. Paint Worklet (Houdini) — evaluated, mostly rejected

FilmCraft's UI is one egui canvas rendered by wgpu; CSS Painting API worklets paint
DOM element backgrounds and borders and **cannot draw inside that canvas or accelerate
anything egui does**. The honest verdict:

- **No value for in-app jank.** Timeline, monitors, scopes and panels are canvas
  content; no paint worklet touches them. Spending effort here would be decoration.
- **Marginal value at the page shell.** The loading overlay and the "FilmCraft
  stopped working" splash in `index.html` are DOM; a paint worklet could render a
  static branded backdrop there. It is not worth a worklet registration and a fallback
  path for non-supporting browsers.
- The worklet that matters for this app is the **AudioWorklet**, already in use —
  and §3's shared ring is its real upgrade. (The Animation Worklet, the other Houdini
  API that could theoretically have composited UI animation, never shipped broadly;
  don't plan on it.)
- If DOM-native surfaces ever appear alongside the canvas (a web-only share sheet,
  settings rendered in HTML), paint worklets become a reasonable way to draw
  waveforms/thumbnails off the main thread — re-evaluate then. Until then:
  **OffscreenCanvas in workers (§2) is the "paint off the main thread" mechanism for
  this codebase**, and that is where the effort goes.

### Also considered

- **`scheduler.postTask` / `scheduler.yield`** for the remaining main-thread work
  (import retries, save serialisation): yield at frame boundaries instead of the
  current ad-hoc budgets. Cheap, incremental, Chromium-first with a `setTimeout`
  fallback.
- **OPFS `FileSystemSyncAccessHandle`** in workers: synchronous, fast file I/O that
  only exists in workers — the right home for the recovery journal and media copies
  once §2's workers exist (today's journal writes land on the async API from the UI side).
- **WebTransport / WebSocket streaming of media**: out of scope; media are local by
  design ("Nothing is uploaded anywhere", web.md).

## Sequencing

| Phase | Work | Depends on | Expected effect |
|---|---|---|---|
| 0. Measure | Extend `apps/filmcraft-web/tests/smoke.mjs` with frame-time capture during demo playback (shown/dropped via `perf.stats` over `window.filmcraft`); record p50/p95 frame time, long tasks > 50 ms, seek latency, audio underruns, on a fixed demo project | — | A baseline every later phase is judged against; headless GPU emulation numbers are indicative only, so keep a real-machine run in the checklist |
| 1. SIMD + import path | wasm SIMD kernels + `--enable-simd`; WebGPU texture import for WebCodecs frames; adaptive pump slicing (spend only the frame budget that remains after the egui pass) | — | Cheapest large win: decode and conversion get faster with no deployment change; scrub and playback smooth out before threads exist |
| 2. Threaded build | Nightly `build-std` in xtask, dual artifacts, loader branches on `crossOriginIsolated`; FrameServer worker pool on; rayon parallel again; fixed preallocated memory | COOP/COEP in production hosting | The structural fix: UI thread does input + paint only; pump caps and their overshoot disappear |
| 3. Workers + shared buffers | Export, indexing, thumbnails/waveforms, proxies/previews, OPFS copies move to task workers; SAB audio ring; shared chunk cache; `Atomics.notify` on chunk arrival | 2 | Long jobs stop taxing interaction; web-only feature errors (proxies, previews, mask tracking) start closing |
| 4. GPU completion | Compute scopes; worker-side WebGPU for previews; CPU-compositor fallback confined to workers | 2, 3 | Colour work at full refresh; the WebGL2 path degrades in frame rate, not in UI responsiveness |
| 5. Hardening | Safari/Firefox matrix on the fallback build, memory ceilings, pool sizing on low-core machines, perf CI gate from phase 0 | 2–4 | The fallback stays a supported product, not a legacy path |

## Risks and constraints

- **Nightly toolchain** for the web build only, pinned like the wasm-bindgen CLI is
  today; a nightly regression must never block desktop releases (separate CI job,
  artifacts published independently).
- **Cross-origin isolation is a hosting requirement.** If production hosting cannot
  send COOP/COEP, phase 2+ benefits vanish and phases 0–1 are the whole plan — which
  is why they are sequenced first.
- **Testing matrix doubles** (threaded × fallback, WebGPU × WebGL2). The codec parity
  tests and the browser smoke test are the gates; the threaded build must produce
  bit-identical frames to the single-threaded one.
- **4 GiB wasm memory ceiling** is unchanged by threads: caches (frame budget,
  chunk LRU) keep their current caps; shared memory makes the caps go further, it
  does not raise them.
- **Oversubscription**: pool size must leave cores for the browser's own compositor,
  the WebCodecs hardware-decode threads and the audio render quantum, or the jank
  moves rather than disappears.

## Success criteria

On the phase-0 demo project, in a current Chromium on a mid-range laptop:

- Playback at 1080p: zero dropped frames over the demo's full play, p95 frame time
  under 16.7 ms, no long task over 50 ms attributable to FilmCraft during playback.
- Scrubbing: playhead follows the pointer with no visible lag; seek lands within one
  frame interval once chunks are cached.
- Export running in the background: the UI stays at 60 fps while editing another
  sequence; export throughput at least matches today's dedicated-pump throughput.
- Audio: zero worklet underruns during playback at 1080p with effects, ring latency
  bounded by the existing 250 ms mix-ahead.
