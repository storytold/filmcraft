# filmcraft-prores

Clean-room Apple ProRes decoder and encoder in pure Rust, written from the public [SMPTE RDD 36:2022](https://pub.smpte.org/doc/rdd36/20220909-pub/rdd36-2022.pdf)
bitstream description. Layer L0: depends only on `filmcraft-bitstream`, `thiserror` and (optional,
feature `threads`, default on) `rayon`. Builds for `wasm32-unknown-unknown`; no `unsafe`.

## API

```rust
use filmcraft_prores::*;

// Decode one QuickTime sample (one ProRes frame).
let frame: Frame = decode_frame(&sample)?;                 // 10-bit 4:2:2 / 12-bit 4:4:4
let f10 = decode_frame_with(&sample, &DecodeOptions { bit_depth: Some(10), threads: true })?;
decode_frame_into(&sample, &DecodeOptions::default(), &mut frame_buf)?; // reuses planes
let hdr: FrameHeader = probe(&sample)?;                    // header only

// Encode (progressive).
let mut enc = Encoder::new(Profile::Hq, 1920, 1080);       // or Encoder::with_config(EncoderConfig { .. })
let sample: Vec<u8> = enc.encode(&frame)?;                 // the QuickTime sample for fourcc Profile::fourcc()
```

`Frame { width, height, chroma: ChromaFormat::{Yuv422, Yuv444}, bit_depth, y, cb, cr: Vec<u16>,
alpha: Option<Vec<u16>>, interlace: Interlace, color: ColorInfo { primaries, transfer, matrix },
aspect_ratio, frame_rate_code }` holds planar samples with no row padding (`chroma_width()` is
`ceil(width/2)` for 4:2:2). The colour codes are H.273 code points straight from the frame header.

`Profile` = `Proxy` (`apco`), `Lt` (`apcs`), `Standard` (`apcn`), `Hq` (`apch`), `P4444` (`ap4h`),
`P4444Xq` (`ap4x`). The frame itself doesn't say which profile it is (only the container fourcc
does), so the decoder doesn't need to know.

## Decoder

- Frame header (dimensions, chroma format, interlace mode, aspect/frame-rate codes, colour
  description, alpha type, optional luma/chroma quantisation matrices), picture header(s), slice
  table, slices.
- Entropy decoding: RDD 36's adaptive Golomb-Rice/Exp-Golomb codebooks. DC is coded as the first
  value plus sign-predicted differences. AC is run/level coded, interleaved across all blocks of a
  slice component.
- Progressive and interlaced scan orders. Interlaced frames are two field pictures (top or bottom
  field first) of `ceil(height/32)` macroblock rows each.
- Dequantisation `F = level × qmat[i] × qscale(qidx)` (qscale = qidx up to 128, then
  `128 + 4·(qidx−128)`).
- IDCT: the exact 2-D orthonormal inverse DCT, evaluated separably in `f32`. It uses even/odd
  symmetry, skips zero rows and fills DC-only blocks directly. Its error is below 2⁻⁸ in the
  12-bit domain. The IDCT output `f` is the 12-bit-domain sample minus 2048. The output at depth
  `d` is `floor(f·2^(d−12) + 2^(d−1) + ½)`, clipped to `[4, 2^d − 5]` for d ≥ 10 (the four
  lowest/highest codes are SDI reserved values; this matches common decoders at 10 and 12 bits)
  and to `[1, 2^d − 2]` below 10 bits.
- 4:2:2 chroma blocks per macroblock are top/bottom. 4:4:4 chroma blocks run TL, BL, TR, BR,
  while luma runs TL, TR, BL, BR.
- Alpha (8- or 16-bit coding): lossless difference/run coding in raster order over each slice's
  16 lines. The starting value is all-ones. Output is scaled to the output depth: 8-bit coding is
  bit-replicated, 16-bit coding is truncated.
- Default output depth: 10 bits for 4:2:2 and 12 bits for 4:4:4. Any depth from 8 to 16 is
  available on request.
- Slice-parallel: each macroblock row, covering both fields of interlaced frames, is decoded by a
  rayon task directly into the output planes.
- Robustness: every size field is validated before allocation, codewords longer than 56 bits are
  rejected, and arithmetic is overflow-safe. Mutated streams return `Err`, never panic (see
  `tests/it/fuzz.rs`).

## Encoder

- Progressive ProRes 422 (Proxy/LT/Standard/HQ) and 4444/4444 XQ. For 4444 the frame's alpha
  plane is coded with 16-bit alpha coding by default (`EncoderConfig::encode_alpha`).
- Input: planar `u16` at any depth from 8 to 16 (normally 10). The chroma format must match the
  profile. Edge macroblocks are padded by replicating the edge.
- Flat quantisation matrices (all 4, not transmitted), 8 macroblocks per slice by default
  (`log2_slice_mbs`), encoder id `fmcr`.
- Rate control targets Apple's published nominal rates (Mbit/s at 1920×1080, 29.97 fps: 45, 102,
  147, 220, 330, 500). These are converted to bits per macroblock (184, 417, 601, 900, 1349,
  2045), so frame targets scale with frame area (`Encoder::target_frame_bytes`). Alpha comes on
  top of the target. Every slice is transformed once. Then a frame-level binary search finds the
  smallest uniform quantiser index whose frame fits the budget, and as many slices as the rest of
  the budget allows get one finer step. Detailed content lands within 0.1% of the target. Simple
  content uses qidx 1 and comes in under the target. Every slice is kept within the 16-bit
  size fields.

## Accuracy (vs ffmpeg as an external oracle)

`tests/it/oracle_decode.rs` decodes 29 fixtures made by ffmpeg's `prores_ks` and `prores_aw`
encoders. They cover every profile, testsrc2/mandelbrot/SMPTE bars at 1920×1080, 1918×1080,
1280×720, 640×360, 352×288, 70×38 and 333×201, interlaced 720×486 TFF/BFF and 1080i, 2-MB slices,
and 4444 with 8- and 16-bit alpha. The results are compared with ffmpeg's decode at the native
depth (10-bit 4:2:2, 12-bit 4:4:4). **Every fixture is within ±1 LSB.** Mean absolute error is
0.0001–0.03 LSB for 422 and up to 0.15 LSB for 12-bit XQ, and 97–99.99% of samples are
bit-identical (86% for 12-bit XQ mandelbrot). Alpha is always bit-exact. The remaining
differences are the reference decoder's integer-IDCT rounding. This decoder is closer to the
exact IDCT and does not reproduce that approximation.

`tests/it/oracle_encode.rs` encodes 1920×1080 frames with every profile (testsrc2, mandelbrot and
noisy content; 4444 with an alpha disc) and writes them into a minimal test-only MOV. ffmpeg then
decodes them with `-xerror`, with no errors, and agrees with this decoder within ±1. HQ PSNR is
69.9/69.2 dB (Y/C) on mandelbrot at 100% of the target rate. Busy content lands at 99.9–100.4%
of the target.

## Alpha bitstream conformance

Every alpha difference is followed by a run code, including a final singleton
(RDD 36:2022 §5.3.3, Tables 12–14). Omitting the final run produces a truncated
alpha payload: FFmpeg may accept it, while macOS native HD decoding fails with AVFoundation `-11821` / underlying `-12137`. This affected
both 4444 and 4444 XQ exports, including EffectCraft's alpha output
([effectcraft#293](https://github.com/storytold/effectcraft/issues/293)).

The encoder tests check exact singleton codewords at 8 and 16 bits, run-length
boundaries through 2048 and split runs, final-value bit alignments, and complete
encoded slices for both profiles and all four slice widths. A separate,
bounds-checked parser follows the published syntax and requires every run and
zero alignment bit; round-tripping through a permissive decoder alone cannot
catch this defect.

Native validation on Apple M5 / macOS 27.0.1 used 14 synthetic 1920×1080 clips:
opaque, transparent, gradient, changing last pixel, moving block, noisy alpha,
and RGB-only, for each 4444 profile. Before the fix, the gradient, last-pixel and
noise cases failed in AVAssetReader (six clips); afterwards all 14 decoded all
six frames. FFmpeg's decoded frame hashes are unchanged by the fix. Small
128×32 clips passed even before the fix. Resolve itself and the reporter's
M1 Pro / macOS 15.8 environment were not available for this check.

EffectCraft 0.16.0's complete export pipeline was also tested using an animated
synthetic PNG with varying alpha: 4444 / XQ × straight / premultiplied × motion
blur off / on (eight six-frame HD movies). All eight failed native decoding with
the pinned encoder, and all eight passed with this codec patched in. FFmpeg's
decoded frame hashes remained identical for every before/after pair.

## Performance

Measured with `cargo test --release -p filmcraft-prores --test it perf:: -- --ignored --nocapture` on
an M4 Pro (14 cores) that was heavily loaded by other jobs, so these are lower bounds. A 1080p HQ
frame at the full nominal rate (918 KB) decoded at up to 318 fps threaded and 53 fps on one
thread. A prores_ks HQ mandelbrot frame (657 KB) decoded at 390 fps threaded. Encoding a 1080p
HQ frame takes about 40 ms threaded.

## Gaps

- The encoder is progressive-only (the decoder handles interlaced), uses flat quantisation
  matrices (no perceptual matrices), and has no trellis/RDO.
- Output is not bit-exact with ffmpeg's integer IDCT; it stays within ±1 LSB of it (see above).
- The 4:2:2 decoder output is fixed at the coded 10-bit precision unless another depth is
  requested; there is no direct output of packed formats (v210, etc.).
- No container handling: frames are QuickTime samples, and demuxing/muxing lives in
  `filmcraft-isobmff`.
