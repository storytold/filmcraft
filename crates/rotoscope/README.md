# Native rotoscope inference

L2, with no UI, decoder, HTTP client, Python embedding or foreign ML runtime. The optional
`inference` feature uses RTen 0.27.0 (MIT OR Apache-2.0) to execute the actual pinned
EdgeTAM and ViTMatte Small neural graphs on the CPU. Default and WASM builds exclude RTen.
The engine supplies decoded RGBA, exact source ticks and validated object/brush points.

Model preparation lives in `experiments/rotoscope/export_native.py`. Python/PyTorch are
offline conversion and conformance tools only. Six ONNX graphs use opset 23, with real
rotary attention, dynamic temporal memory and dynamic matte tile dimensions. Rust does
image normalization, prompt grouping, bidirectional propagation, temporal memory,
trimaps, padded tiled matting, bicubic absolute position interpolation, definite-pixel
locking and bounded 16-bit RLE. The exporter preserves the checkpoint architecture;
prompt-count-specific graphs preserve SAM's multimask/stability selection.

Sources and revisions:

- [Meta EdgeTAM](https://github.com/facebookresearch/EdgeTAM), Apache-2.0, source
  `7711e012a30a2402c4eaab637bdb00a521302c91`; checkpoint revision
  `14d7ecc48c656b94e5184519f698cd5386c5a2bf`, SHA256
  `ed2d4850b8792c239689b043c47046ec239b6e808a3d9b6ae676c803fd8780df`.
- [HUST ViTMatte Small](https://huggingface.co/hustvl/vitmatte-small-distinctions-646),
  Apache-2.0, revision `6a0e75d7214b01f4d1163ede0f15b23afbbd480b`, SHA256
  `7bbe217ec9d6945679b6191ccd071006a3975675326312df1ecb5518488049a8`.
- [ONNX operator specification](https://onnx.ai/onnx/operators/), opset 23;
  [RTen 0.27.0](https://docs.rs/rten/0.27.0/rten/).

Graphs and footage stay in ignored `target/rotoscope`, with attribution sidecars.
`models.rs` pins each graph's exact length/hash independently of the local manifest.
`experiments/rotoscope/native-models.json` records the reproducible export package.
Changing checkpoints, exporter or conversion dependencies requires new pins and numerical
conformance checks. All model loads/jobs catch unwinds and return actionable errors.

Limits are 720 pixels on the longest side, 120 frames, 40 million aggregate pixels,
512 points, 16 prompted frames, edge bands 1..32 and bounded temporal memory. Feature,
tracking and alpha caches retain at most 192 MiB / 40 MB / 80 MB respectively. Brush
changes reuse tracking and unchanged alpha. Cancelled jobs publish no new alpha entries.
Single-frame previews use at most four model threads and serial matte tiles. Multi-frame
jobs use up to eight model threads and four concurrent matte tiles sharing that thread
pool, bounded by available CPUs. `FILMCRAFT_AI_ROTO_THREADS` overrides the neural budget;
`FILMCRAFT_AI_ROTO_TILE_WORKERS=1` reduces memory by restoring serial tiles independently
of that budget (allowed range 1..min(4, sequence threads)). Position embeddings reuse a
four-entry, 6 MiB cache. Worker failures/cancellation join all outstanding work before
returning; incomplete alpha is not published. Model kernels and preprocessing are
optimized in debug builds. See [Colab measurements](../../experiments/rotoscope/PERFORMANCE.md)
for timing, peak-memory tradeoffs and the separate T4 research probe.

```sh
cargo +1.95 test -p filmcraft-rotoscope --features inference
cargo +1.95 run -p filmcraft-rotoscope --features inference --example oracle -- \
  target/rotoscope/models/native
```

The oracle optionally accepts up to 16 fixture/result pairs, reusing one runtime. Each bounded JSON fixture (dimensions, RGBA files, exact
times/end, points, mode and band) may reference one existing little-endian u16 alpha plane
for local refinement. Soft alpha outside the painted area is preserved exactly. Compare against the original PyTorch
predictor using the same raw RGBA and half-pixel bilinear preprocessing. The former
Python editor bridge reencoded PPM through JPEG/PIL; that convenience loader is a
different input adapter. The Rust path avoids its compression and transport entirely.

This is CPU inference. Metal/CUDA acceleration, temporal alpha regularization, optical
flow for brush propagation, difficult-occlusion quality and longer/4K jobs remain open.
See the experiment's `RESULTS.md` for measured limits, rather than treating a language
port as a performance claim.
