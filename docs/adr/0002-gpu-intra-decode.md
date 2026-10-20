# ADR 0002: intra-only codecs decode on the GPU (ProRes, DNxHD / DNxHR)

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

## DNxHD / DNxHR (2026-10-20)

VC-3 has no slices: a macroblock scan line is one entropy-coded run (the DC predictors run
through it, and its macroblocks have no offsets), so a 4K frame has ~135 independent units
instead of ProRes' ~4000.

- **Measured first, one invocation per scan line:** correct, but 27 ms per 4K DNxHR HQ frame
  through the compositor, against 14.5 ms for the CPU path. Everything but the entropy pass
  (upload, IDCT, output) took 1.9 ms; a single GPU invocation walking ~200 000 bits of dependent
  VLC lookups is the bottleneck, and the VLC tables in workgroup memory changed little. A split
  (entropy on the CPU, transform on the GPU) was ruled out by measurement too: entropy is about
  half of the CPU decode (8.4 of 13.2 ms with all cores), so it would save ~30 % and keep the CPU
  busy.
- **Decision: every scan line is cut into 1024-bit segments decoded in parallel and stitched
  together** (`dnx.wgsl`: `sync`, `trans`, `scan`, `decode`, then `idct`). Each segment takes as
  its candidate block boundary the end of the first block a VLC parse finds in it, started 256
  bits early; for every hypothesis of the block index there, it parses whole blocks until one
  ends exactly on a later segment's candidate, recording the blocks, the DC difference sums and
  the last quantiser scale. One invocation per line then follows the chain from the line's start;
  a parse from a true state lands only on true boundaries, so every segment reached gets its
  exact decoder state, and the reached segments decode their blocks in parallel. On ffmpeg's and
  our streams (DNxHD 1080p/i, 720p, thin rasters; DNxHR LB to HQX, up to UHD) 90–97 % of the
  candidates are true boundaries, and no chain needed to pass more than 4 candidates (the shader
  allows 8; a line whose chain breaks counts as corrupt and shows its CPU decode).
- **Scope:** Y'CbCr 4:2:2 without alpha, 8 / 10 / 12 bits, progressive or field-coded
  (`filmcraft_dnx::frame_layout`, which parses the coding units and scan indices with the CPU
  decoder's own code). 4:4:4 (RGB / adaptive colour), alpha, CID 1260 (macroblock-adaptive field
  coding) and 4:2:0 (which the shaders handle but no available encoder writes, so nothing tests
  it) are decoded on the CPU. 8-bit frames fall back to `PixelData::Yuv8`.
- **Result:** 4K DNxHR HQ 15.2 → 4.5 ms per frame, 4K HQX 16.4 → 4.5 ms, 1080p HQX 4.3 → 2.0 ms
  (RTX 4090, Ryzen 7 7800X3D); the output matches the CPU decoder like ProRes' (`dnx_tests.rs`:
  our encoder's frames and 18 ffmpeg fixtures, at most one code apart on rounding ties).
