# ADR 0002: intra-only codecs decode on the GPU (ProRes first)

- **Status:** proposed (2026-10-19)

## Context

ProRes (and DNxHD / DNxHR) are what editors cut with. FilmCraft decodes them on the CPU, fast:
4K ProRes HQ at ~210 fps on a Ryzen 7 7800X3D with all cores. But playback pays for more than the
decode: the 4K frame is 33 MB of 16-bit planes that the CPU then converts to half floats and
uploads. Measured through the compositor (CPU decode + conversion + upload + composite, until the
GPU is done), a busy 4K ProRes HQ frame at the full nominal rate costs 14.7 ms, 4444 19.4 ms.

ProRes is built for parallel decoding: every frame is intra, and every slice (8 macroblocks;
~4000 per 4K frame) is entropy-coded on its own. No hardware decoder outside Apple silicon reads
it, but the GPU's compute cores can: one invocation per slice component for the entropy stage,
one per 8×8 block for the inverse DCT.

## Decision

The GPU compositor decodes ProRes itself, in WGSL compute shaders, from the bitstream.

- **`PixelData::Coded(Arc<CodedImage>)`** (`filmcraft-frame`, L1): a picture in its compressed
  form — format, bytes, chroma, bit depth, alpha — with a CPU fallback the decoder supplies
  (decoded once, then cached). Every CPU code path resolves it through
  `VideoFrame::cpu_frame`, so nothing can see a missing or wrong picture, only a slower one.
  `box_decimated` leaves it whole: decoding it on the GPU costs less than the CPU decode a
  reduction needs.
- **Who gets coded frames:** the `ProResDecoder` (`filmcraft-codecs`) hands them out under the
  `with_gpu_frames` hint, which the monitors' plan jobs set when they composite on the GPU.
  Everything else (thumbnails, export, CPU effects, scopes) gets planes as before.
- **Parsing stays on the CPU and in the codec crate:** `filmcraft_prores::frame_layout` parses
  and checks the frame and picture headers, the slice tables and every slice header with the CPU
  decoder's own code (factored out of `decode_frame_into`). A frame that does not parse is decoded
  on the CPU.
- **`filmcraft-gpu::intra`** (L3, no `unsafe`): uploads the bitstream, the slice records and the
  constants (quantisation matrices, scan, codebooks, the DCT basis and the code → half-float
  table the CPU upload path uses), runs `entropy` and `idct` (`prores.wgsl`) in batches whose
  coefficient buffer fits the device's storage-binding limit, and copies the packed output into
  R16Float Y / Cb / Cr / A textures — the textures the upload path makes from CPU planes, so
  colour management, effects and blending are untouched.
- **Fallbacks, never a crash:** a device without compute support, a frame above the device's
  texture or buffer limits, or a layout error: the CPU decode is uploaded instead. Inside a slice
  the GPU counts corrupt data in a status word read back asynchronously; the picture is then
  marked and shows its CPU decode (and its error) from the next composite on. The pipelines are
  created inside a validation error scope.

## Consequences

- Through the compositor, until the GPU is done (RTX 4090, Ryzen 7 7800X3D, busy content at the
  nominal rate): 4K ProRes HQ 14.7 → 2.8 ms per frame, 4K 4444 19.4 → 3.8 ms, 1080p HQ 3.6 →
  1.3 ms — and the CPU is free for everything else.
- The GPU decode follows the CPU decoder's arithmetic step by step; compilers may fuse
  multiply-adds, so a sample whose exact IDCT value lies at a rounding tie can come out one code
  apart (~1 in 10⁵–10⁶ samples in the tests). Alpha is bit-exact. The CPU decoder stays the
  reference; `intra_tests.rs` compares every sample of our encoder's and ffmpeg's streams
  (all profiles, interlaced, 8- and 16-bit alpha, odd sizes).
- The coefficient buffer (up to the storage-binding limit, 128 MiB by default) and the output
  buffer stay pooled in the compositor between frames.
- DNxHD / DNxHR is next. Its entropy coding is sequential per macroblock row (DC prediction runs
  through the row), so a 4K frame has ~135 independent units instead of ~4000; whether it decodes
  wholly on the GPU or splits entropy (CPU) and transform (GPU) is decided by measurement.
