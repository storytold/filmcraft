# ADR 0003: intra-only codecs encode on the GPU (ProRes, DNxHD / DNxHR)

- **Status:** proposed (2026-11-07; the project's maintainers decide)
- **Extends:** [ADR 0002](0002-gpu-intra-decode.md), the same codecs in the other direction and under
  the same rules: WGSL compute in `filmcraft-gpu`, the CPU codec stays the reference and the
  per-frame fallback, never a crash, never a wrong file.

## Context

ProRes and DNx exports spend their time in the encoder. Measured before this work, on 2026-10-24
(RTX 4090, Ryzen 7 7800X3D, 100 frames of 3840x2160 25p real camera footage, smart rendering off,
`perf.stats` -> `export.stages`): ProRes 422 HQ took 10.3 s with CPU rendering and 8.8 s with GPU
rendering; DNxHR HQ took 6.5 s and 4.7 s. The encode stage was serial (9.5 / 7.9 s for ProRes, 5.6 /
3.8 s for DNxHR) and `waitMs` was 0: the encoder, not the renderer, was the bottleneck. Both codecs are built for parallel work. ProRes slices (about 4000 per UHD frame) are
coded independently. A DNx scan line is one entropy run, but when encoding every coefficient and
every DC predictor is known before a bit is written, so even DNx's bit layout can be computed in
parallel. No vendor hardware encodes either codec outside Apple silicon (NVENC, AMF and Quick Sync
do H.264, HEVC and AV1), so the GPU's compute cores are the only portable accelerator.

The same measurement session found that the encoders saw only 8 bits for SDR exports (a 10-bit ramp
with 64 luma levels came out of ProRes HQ with 19). A GPU encoder has to take at least 16-bit input,
so the CPU path needed the same fix first.

## Decision

FilmCraft gets GPU encoders for ProRes and DNx in WGSL compute shaders, a second implementation of
the same bitstream writers, chosen per export and per frame, with the CPU encoders as reference
and fallback. The goal is everything on the GPU: colour, alpha, rate control, interlaced, and the
frame staying on the GPU. All of it is implemented. In this ADR, *Step 1* means GPU encoders fed
Y'CbCr planes read back from the renderer, and *Step 2* means the rendered frame stays on the GPU
into the encoder. The goals were: encoding a UHD frame in at most 10 ms, a 100-frame UHD ProRes HQ
export in at most 4 s with Step 1 and at most 2.5 s with Step 2.

### A backend seam in the codec crates

The codec crates (L0, no new dependency) keep everything that decides the bitstream: input checks,
rate control, slice and macroblock headers, frame assembly. Each gets a narrow backend trait for
the per-block, parallel work: `filmcraft_prores::SliceBackend` and `filmcraft_dnx::MacroblockBackend`
(`load`, `bits` at a quantiser, `write`, `alpha`, `choose`). `Encoder::encode_with(frame, backend)`
calls the backend instead of its own functions and encodes the frame itself when the backend
declines, fails or answers inconsistently: counts that do not match, byte lengths that do not
match the bits it measured, quantisers out of range, DNx macroblock headers that disagree with
their quantiser. The encoder checks these parts before it assembles the frame; it does not parse
the assembled frame. `Encoder::encode_resident` does the same for a frame the backend already holds. `ReferenceBackend` (a CPU test double, behind
the codec crates' `test-support` feature, which only other crates' dev-dependencies enable) drives
the seam in tests.

A backend's own rate control (`choose`) is checked differently by the two encoders. ProRes takes
any answer in range: its frames have no fixed size, so an answer that overshoots the budget only
makes a larger frame, and each slice's 16-bit size fields are still checked when it is written.
DNxHR re-measures the answer and keeps it only when the frame fits the budget: a DNxHR frame has
the profile's constant size, and one that does not fit cannot be written.

### `filmcraft-gpu::intra_enc`

L3, no `unsafe`. `ProResGpuEncoder` and `DnxGpuEncoder` implement the backends in `prores_enc.wgsl`
and `dnx_enc.wgsl`: forward DCT per block, per-slice (ProRes) or per-macroblock (DNx) bit counts at
a quantiser, rate control, entropy coding into per-slice regions (ProRes) or the parallel bit
layout (DNx: an exclusive prefix sum of macroblock bit lengths per scan line, one invocation per
output word gathering overlapping macroblocks). Only the coded frame (1 to 4 MB for UHD) is read
back, one command buffer and one read-back per frame. Pipelines are created inside a validation
error scope.

### Byte-identical to the CPU, not just close

The required bar is a decoded PSNR within 0.1 dB of the CPU encoder. The
implementation reaches byte identity: the byte-identity tests assert that at least 90 % of the
frames are identical (leaving room for other drivers), and on the reference GPU 100 % are. Two
tricks make it so:

- **`mul()`**: every product in the DCT and in quantisation goes through
  `bitcast(bitcast(a*b) ^ zero)`, `zero` being a uniform that is always 0. The driver cannot see
  through it, so it cannot fuse multiply-adds that the CPU does not do. Without it, 90 of 160
  ProRes test frames were identical (every 1080p case differed). It costs nothing measurable.
- **`div_rn`**: a correctly rounded `a / b` (ties to even) for a whole `b <= 4095`, by integer long
  division. GPU f32 division is not correctly rounded (Vulkan allows 2.5 ULP). With hardware
  division 96 of 135 DNx test frames were identical.

The fallbacks below stay in force whatever the output: the property is tested, not trusted.

### Rate control on the GPU

The CPU algorithm runs in compute passes without a CPU round trip, reproducing the CPU's
quantisers exactly (ProRes: bisection over `Q_MIN..=Q_MAX` by 16 rounds, then the one-step-finer
refinement and the 16-bit slice-size guard; DNx: uniform `qsf` search in 14 rounds, then the
per-macroblock refinement in the CPU's fixed strided walk). Anything the CPU algorithm would not
survive (slot overflow, sums beyond 32 bits, buffers beyond the device limits) declines the frame
before or after the work, and the CPU encodes it. Rate control first ran on the CPU over
GPU-measured bit counts; that path remains as the answer when the GPU search cannot run.

### Alpha

ProRes 4444 codes alpha on the GPU, one invocation per slice (the CPU's serial run). DNxHR codes
alpha on the GPU as DCT or lossless. When lossless alpha does not fit the frame's budget, that
frame falls back to DCT alpha (the frame header carries the alpha coding per frame), and an
actionable error is raised only if DCT alpha does not fit either. HDR exports with alpha keep
the RGBA8 alpha copy rather than writing a file that claims alpha with opaque alpha. DNx alpha
export has not been verified against Avid's decoder; ffmpeg and our decoder read it.

### Interlaced ProRes

`EncoderConfig::interlace` (top or bottom field first) codes two field pictures, the temporally
first first, with one rate control over both; the GPU codes both pictures from the same planes.
In MOV the `fiel` atom carries 9 (TFF) or 14 (BFF), as Apple writes it. Caveat: ffprobe maps
those to container-level `field_order` `tb` / `bt`; the coded order is read from the decoder
(`top_field_first` per frame). Interlaced ProRes in MXF writes the SeparateFields descriptor.
Interlaced DNxHD export is not part of this work.

### Float hand-off

For SDR exports `EncoderFrame` carries f32 R'G'B' after the output transfer function (the
representation the HDR path uses), with alpha, when the export keeps it, from the RGBA8 picture;
the ProRes / DNx / APV encoders convert from it; 8-bit outputs keep RGBA8. This is based on PR #3928 (JonRC), whose commits keep their authorship. A 10-bit
ramp with 64 luma levels used to come out of a ProRes 422 HQ export with 19; it now keeps 64 of 64
levels through ProRes 422 HQ and DNxHR HQX.

### Registration and selection

The apps register the encoders through `filmcraft_export::register_prores_backend` /
`register_dnx_backend`, as they register the GPU frame renderer. The new crate
**`filmcraft-gpu-export`** (L4) holds that glue once for both apps and the tests: the
export renderer, the encoder factories and the paired registration. The existing *Hardware
encoding* export setting (off / auto) governs it: `auto` uses the GPU encoders when a
compute-capable device exists. `filmcraft_export::gpu_render_stats()` counts `encoded`,
`encode_fallbacks`, `resident` and `resident_fallbacks` (not yet in `perf.stats`; `export.stages`
shows the timing). A factory or backend that panics runs under `catch_unwind`; the panic is
still logged by the crash hook (and shown once in the error window, as a caught frame-worker
panic is), the backend is dropped, and the export continues on the CPU.

### Step 2: the frame stays on the GPU

With GPU rendering and a GPU encoder, the rendered frame no longer round-trips through the CPU.
`export_post.wgsl` runs the per-pixel work after rendering on the same device (placement and
scaling with the CPU's weights, output transfer function, video limiter, un-premultiply, matrix,
chroma subsampling, 10-bit quantisation, alpha) into a planes buffer that the encoder reads
directly.

- **Decided once per export**, in `Exporter::set_up`: every pooled renderer and the encoder come
  from the same pair, the settings are supported, sizes fit the device. Otherwise the export is
  today's read-back path.
- **Overlays and HDR select the read-back path** (image / name / timecode overlays have no GPU
  pass; HDR sequences are never composited by the GPU renderer, so there is no resident frame to
  post-process).
- **Per-frame fallback**: a frame the GPU cannot composite, a wgpu error in its error scopes, a
  corrupt GPU-decoded picture, or a backend that declines renders that frame again through the
  read-back path (`resident_fallbacks`).
- **Sticky stop**: the first time the encoder refuses a resident frame, the resident path ends for
  the rest of the export, so a persistent refusal does not render every frame twice.
- Pictures reduced more than 32x, bit depths other than 10, and sizes beyond device limits take
  the read-back path.

### One device, one consequence

The paired renderers live on the encoders' device (adapter limits, instead of the older
renderer's default limits), because a resident frame has to be readable by the encoder. A device
lost or an uncaptured wgpu error retires it for rendering and encoding alike, and the encoders'
device is process-wide: **a lost device disables GPU encoding for the rest of the process**, not
only for the rest of the export. The export then completes on the CPU. A restart gets the GPU back.

### Correctness bar and fallbacks, as implemented

- **Reference:** the CPU encoder. Required for every GPU-encoded frame: it parses with
  `frame_layout`, decodes with our CPU decoder and ffmpeg, fits the profile's size limits, and its
  decoded picture is within 0.1 dB PSNR of the CPU's. The measured result is stronger: 160 of
  160 progressive and 274 of 274 interlaced ProRes frames and 135 of 135 DNx frames are
  byte-identical to the CPU encoder, across profiles, depths, chroma formats, alpha, odd sizes
  (1x1, 17x13, 1919x1079) and 1 to 8 macroblocks per slice.
- **Per frame:** a frame whose backend output is inconsistent (lengths, quantisers, DNx
  macroblock headers: the encoder checks these, it does not parse the assembled frame), exceeds a
  limit, or whose backend failed or panicked is encoded again on the CPU. Every frame is intra,
  so a file may mix both.
- **Up front:** no compute support, a frame above the device's storage or texture limits (8K
  DNxHR 444, 16K), a pipeline that fails validation or an unsupported configuration: the CPU
  encoder takes the export or the frame.
- **Limits:** buffer sizes are checked against `max_storage_buffer_binding_size` and
  `max_buffer_size` before creation; checked arithmetic and `get()` for every index derived from
  frame sizes or read-backs.

## Consequences

Measured on the reference machine (RTX 4090 / Vulkan, Ryzen 7 7800X3D), release build:

- **The encoder alone**, ProRes HQ and DNxHR HQ at 3840x2160, 30 frames, median per frame, with GPU
  rate control: ProRes 42.4 ms CPU -> 10.6 ms GPU (3.9x); DNxHR 57.9 ms CPU -> 8.0 ms GPU
  (7.2x). Medians of three runs.
- **UHD exports, 100 frames, wall time** (median of three; `export.stages`):

  | Export | CPU | GPU, read-back | GPU, resident |
  |---|---:|---:|---:|
  | ProRes 422 HQ | 6.98 s | 3.83 s | **0.99 s** |
  | DNxHR HQX | 9.14 s | 4.06 s | **1.05 s** |

  Resident exports are encode-bound: the stage is the GPU encoder's own read-back of the coded
  frame plus the CPU's frame assembly. The goals above are met (3.83 s with Step 1, 0.99 s with Step 2)
  except the per-frame encode of ProRes HQ, which sits at about 10.6 ms.
- **GPU rate control is not free for DNx.** Over GPU-measured bits with the CPU algorithm on the
  CPU, DNxHR HQ took about 7.5 ms per 4K frame; with the search on the GPU it takes about 8.2 ms
  (ProRes: within noise). The rounds are sequential single-quantiser measurements. Accepted
  because rate control on the GPU is a stated requirement, and recorded here. An idea not done: measure
  both outcomes of the next question per round, which would halve the live rounds.
- Two encoders per codec to keep in step. The byte-identity tests make drift visible at once.
- A GPU that is busy rendering is busy encoding too; on a weak GPU `auto` may be slower than the
  CPU (not measured).

### Known gaps

- Tested only on NVIDIA (RTX 4090, Vulkan). AMD, Intel and Apple GPUs are untested;
  another GPU's `pow` may differ by one code in the sRGB curve, so the post-pass test allows one
  code on at most 2 % of samples (0 differed on the 4090).
- HDR exports and exports with overlays do not take the resident path.
- DNx alpha export is not verified against Avid's decoder.
- ffmpeg and our decoder differ by up to 9 of 4095 codes on 12-bit DNxHR 444. This predates the
  GPU work (it shows with the CPU encoder), is not understood, and is an interop risk for files we
  write.
- ProRes 4444 / DNx lossless alpha from an 8-bit source goes 8 -> 10 -> 8 bits and can differ from
  the source by one code.
- Interlaced smart render is not part of this work: smart render itself is not on this branch, and
  open PR #3074 covers smart rendering.
- VideoToolbox's ProRes engines on Apple silicon stay a possible later addition behind the same
  seam.
