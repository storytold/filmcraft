# Native rotoscope performance — Colab, October 16, 2026

The native CPU path keeps the same pinned EdgeTAM and ViTMatte models, resolution,
trimaps, tile boundaries, 64-pixel context padding and 16-bit alpha. No frame
skipping, reduced precision, new weights or product dependencies are involved.

## Implementation

- Single-frame previews use at most four inference threads and one matte tile at a
  time. Multi-frame generation uses at most eight inference threads, bounded by
  available logical CPUs. `FILMCRAFT_AI_ROTO_THREADS` still overrides both budgets.
- Up to four independent matte tiles run together, sharing the job's existing
  inference thread pool. Results are assembled in source order. Workers are joined
  on success, cancellation, model errors and panics; incomplete alpha is not cached.
- Bicubic model position embeddings are reused for matching tile dimensions in a
  four-entry, 6 MiB cache. They do not depend on the image, points or brush.
- `FILMCRAFT_AI_ROTO_TILE_WORKERS=1` restores serial tile execution to reduce peak
  memory, independently of the neural thread budget. Values must be 1 through the
  smaller of four and the configured sequence thread count. Previews stay serial.

## CPU measurements

All compilation, decoding, inference and validation ran through the Colab CLI,
not on the contributor's Mac. CPU: eight logical threads / four physical cores,
Intel Xeon Broadwell at 2.20 GHz, approximately 51 GiB RAM. Rust 1.95 / RTen 0.27.0;
release builds used no LTO, 16 codegen units and `-C target-cpu=native` for both
baseline and candidate. These are benchmark/CI overrides, not changes to the
shipping Cargo profile.

Baseline source: `f1d7644151b801c9135dc548aaf08ce8251144ea`. The final adjacent pair
used the same 30 frames, 720×405 RGBA input, Include point, edge band 8, model hashes
and CPU VM. No build or other inference job was running on that VM. The separate
T4 session had been terminated. Times below are native inference plus JSON result
serialization; model loading, input-file reads and video decoding are excluded.

| Final adjacent pair | Previous default | New default |
| --- | ---: | ---: |
| Inference threads / matte workers | 4 / 1 | 8 / 4 |
| First 30-frame Quality request | 93.613 s | 77.648 s |
| Refinement and serialization after tracking | 51.499 s | 39.243 s |
| Identical repeat, using caches | 0.530 s | 0.529 s |
| Sampled peak process RSS, including models/input | 875 MiB | 1,400 MiB |

That pair used **17.1% less total generation time** (1.21× throughput), and **23.8%
less refinement time**. The tradeoff is higher peak RAM for concurrent activation
buffers. Use the tile-worker override above when memory matters more than throughput.
These are Colab Linux measurements, not measured Mac editor timings.

Shared-VM timings varied. The three original-default full requests took 115.658,
101.887 and 93.613 seconds; the three new-default requests took 111.255, 77.666 and
77.648 seconds. The initial thread-only comparison took 115.658 seconds at four
threads versus 101.486 seconds at eight. These samples are too few and variable for
a general performance guarantee; the final adjacent pair is reported explicitly,
rather than presenting the fastest candidate against the slowest baseline.

A separate short-clip sweep held neural threads at eight, alternated execution
order, and used fresh processes/caches each time:

| Scheduling | Three-frame request, two runs |
| --- | --- |
| Baseline serial | 8.183 s, 8.147 s |
| Candidate serial tiles | 8.455 s, 8.181 s |
| Candidate two tiles | 7.749 s, 7.799 s |
| Candidate four tiles | 7.132 s, 7.447 s |

Four workers reduced mean request time by 10.7% at the same neural thread budget.
Position caching alone did not show a meaningful speedup in this short test; its
purpose is to avoid repeated deterministic preparation across a longer clip.

All full-clip outputs above were byte-identical, including source times and alpha
runs. Include/Exclude, multiple prompted frames, reverse propagation, brush edits,
Fast mode and a local correction over existing soft alpha also matched the old
runtime exactly. Alpha outside the painted area was unchanged. This verifies
preservation of the existing result, not segmentation accuracy against ground truth.

Fixture: first 30 frames of Amada44's CC BY-SA 3.0
[dog video](https://commons.wikimedia.org/wiki/File:REM_-_Rapid_eye_movement_sleep_of_a_dog.webm),
resized by external FFmpeg on Colab to 720×405. Frames are at 25 fps; the Include
point is `(195.1036, 141.64388)` on frame zero. Media, models, raw alpha and detailed
logs stay outside Git under `target/rotoscope/perf-colab/`. Full-clip serialized
alpha SHA-256: `51bc795c9559ef70fce7a31e3b0f20d84fb35c9ea0d1e3962a0e562dbb3a7aed`.

## Separate T4 feasibility probe

This was a research-only ONNX Runtime CUDA test, **not a new FilmCraft backend**.
FilmCraft remains pure Rust / RTen CPU inference. It would require a separate
native GPU implementation and broader validation to realize this in the editor.

The unchanged, SHA-verified ViTMatte graph processed one representative 448×416
padded tile and its actual normalized image/trimap and position inputs. ONNX Runtime
1.31.0, CUDA on Tesla T4, FP32, TF32 disabled; fixed tile dimensions specialized
through the runtime. The measured calls include host/device transfers.

- Model/session loading: 0.752 s; first tile: 0.857 s.
- Twelve warm tile calls: median **0.0460 s** (range 0.0457–0.0542 s).
- Native RTen eight-thread serial probe: approximately 0.81–0.93 s per matching tile.
- Profiling confirmed all executed kernels in the successful probe used CUDA.
- Against native FP32 output: maximum absolute alpha difference `7.57e-6`, mean
  `2.62e-8`, 99th percentile `2.98e-7` on this tile.

The initial ONNX Runtime 1.23.2 probe silently assigned convolutions and some resize
work to CPU and took approximately 0.78 s/tile. Specializing shapes alone did not
fix it. Updating the **research runtime** resolved that fallback; neither runtime
was added to FilmCraft. This is why checking the actual execution provider matters.
A single warm tile is not an end-to-end full-clip GPU benchmark or a hair-quality
benchmark. GPU memory use, temporal behavior and broader image coverage remain open.
The T4 session was stopped after collecting these results.

## Reproduction and checks

Build the native oracle on the benchmark host, prepare the pinned models, and use
the bounded JSON fixture format described in `crates/rotoscope/README.md`:

```sh
cargo build --release -p filmcraft-rotoscope --features inference --example oracle
python3 experiments/rotoscope/benchmark_native.py \
  --oracle target/release/examples/oracle --models /path/to/native-models \
  --fixture /path/to/dog30-quality.json --threads 4 8 --repeats 3 \
  --output /path/to/new-benchmark-directory
```

Use `--tile-workers 1`, `2` or `4` to compare memory/throughput independently of
thread count. Each fresh process issues one new request and one identical cached
request; the report keeps both timelines separate and asserts cached output identity.
Build the baseline separately to compare versions. Compare serialized alpha hashes
as well as elapsed time; do not compare a cached run with a fresh run.

The audio gate investigation and optimizations are reviewed separately from rotoscoping.
The measurements above predate the October 19 scope cleanup; they are historical
measurements of the unchanged native inference implementation.
