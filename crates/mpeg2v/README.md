# filmcraft-mpeg2v

Clean-room MPEG-2 video and MPEG-1 video decoder in pure Rust. Layer L0: depends only on
`thiserror` and (optional, feature `threads`, default on) `rayon`. Builds for
`wasm32-unknown-unknown`; no `unsafe`. No GPL/LGPL code (FFmpeg, libmpeg2, libavcodec) was
consulted; FFmpeg is used only as an external fixture generator and test oracle.

## Specifications

| Document | Edition | Used for |
|---|---|---|
| ITU-T H.262 \| ISO/IEC 13818-2 | 02/2000 (second edition, with Amd. 1 and Cor. 1/2 as incorporated in the 2000 text) | sequence / GOP / picture headers and extensions, slices, macroblocks, VLC tables (Annex B), inverse quantisation and mismatch control (§7.4), IDCT (Annex A), motion vectors and prediction (§7.6) |
| ISO/IEC 11172-2 | 1993 | MPEG-1 video: slices spanning rows, MPEG-1 escape codes, oddification, full-pel vectors, D pictures |
| IEEE Std 1180 | 1990 | IDCT accuracy test procedure (run as a unit test) |
| ISO/IEC TR 13818-5 (MPEG Software Simulation Group decoder behaviour) | 1997 | two decoder behaviours the normative text leaves open (see "Interpretations") |

## API

```rust
let mut dec = filmcraft_mpeg2v::Decoder::new();
for (i, au) in filmcraft_mpeg2v::access_units(&es).into_iter().enumerate() {
    for pic in dec.decode(&es[au], i as i64)? {             // display order
        // pic.y / pic.cb / pic.cr: tight 8-bit planes; pic.chroma: 4:2:0 / 4:2:2
        // pic.field_order(), pic.picture_type, pic.pts, pic.info (SequenceInfo)
    }
}
let rest = dec.flush();
let info = filmcraft_mpeg2v::probe(&es);                    // sequence header + extensions
let au = filmcraft_mpeg2v::scan_access_unit(&es[..]);       // picture types without decoding
```

`decode` takes any chunk holding whole pictures (an access unit from a TS/PS/MXF/MOV sample).
Pictures come out in display order (anchors are held until the next anchor) and carry the `pts`
of the chunk their first field came in. `reset` forgets pictures but keeps the sequence
parameters, so a random-access point without its own sequence header still decodes.

## Decoder

- Main Profile at Low/Main/High-1440/High Level, 4:2:2 Profile (IMX / D-10, XDCAM HD422),
  Simple Profile, MPEG-1 (including D pictures). 4:4:4 is refused (no profile in use for it).
- Pictures above 2^25 luma samples (8192 x 4096; 8K UHD at 7680 x 4320 fits, and the largest MPEG-2 level,
  High, is 1920 x 1152) are refused as `Unsupported`: the 14-bit header fields allow 16383 x 16383, which
  would ask for gigabytes per frame from hostile input.
- Frame pictures with frame and field motion compensation, dual prime, frame and field DCT;
  field pictures with field, 16x8 and dual-prime motion compensation, the second field of a
  P frame predicted from the first; open and closed GOPs; B pictures without a forward
  reference after a broken link are dropped (closed GOP: decoded from the backward reference).
- Both intra VLC tables, zigzag and alternate scan, linear and non-linear quantiser scale,
  quantiser matrices from the sequence header and the quant matrix extension (chroma matrices
  for 4:2:2), intra DC precision 8-11 bits, concealment motion vectors, saturation and
  mismatch control.
- IDCT: the separable 2-D IDCT in `f32` with even/odd symmetry, the DC term added exactly,
  row skipping and a DC-only path; rounded and saturated to [-256, 255]. `ieee_1180_accuracy`
  runs the IEEE 1180 procedure (10 000 blocks × 3 ranges × 2 signs): peak error ≤ 1 and all mean
  / mean-square limits met.
- Motion compensation clamps reference coordinates to the picture (only corrupt streams point
  outside it); a missing reference predicts from mid-gray.
- Slices of different macroblock rows decode in parallel on rayon (each row into its own chunk
  of the frame); MPEG-1 slices, which may span rows, decode in order.
- Corrupt slices are concealed (the rest of the picture still decodes) and counted
  (`Decoder::errors`, `last_error`); nothing panics on truncated or mutated input.

### Interpretations

Two behaviours follow the MPEG Software Simulation Group reference decoder (and FFmpeg), which
the synthetic-stream tests confirmed against FFmpeg:

- Skipped macroblocks in field pictures predict from the field of the same parity, in B
  pictures as well as P pictures.
- Motion vector reconstruction (§7.6.3.1) brings the vector back into range only on the side the
  delta moves it, and keeps the prediction for a zero delta. This differs from the literal
  formula only when the prediction is out of range: a field vector's doubled vertical
  component predicting a following frame vector.

## Accuracy (vs FFmpeg as an external oracle)

MPEG-2's IDCT is not bit-exact by definition (any IEEE 1180-accurate IDCT conforms, and the
encoder's reconstruction drifts from every decoder's). FFmpeg uses an integer IDCT, so frames
are compared within a tolerance: **per frame max |Δ| ≤ 4 and PSNR ≥ 58 dB, at most 3 % of
samples differing**. Frame count, display order, picture types, interlacing and field order are
exact (ffprobe).

`tests/it/oracle.rs` (FFmpeg-encoded elementary streams):

| Fixture | Content | Max Δ | Differing | PSNR |
|---|---|---|---|---|
| m2v_cif_ibbp | 352×288 progressive IBBP, Main@Main | 2 | 0.60 % | 70.3 dB |
| m2v_576i_tff | 720×576 interlaced TFF, field MC/DCT, alternate scan | 2 | 0.33 % | 73.0 dB |
| m2v_480i_bff_vlc1 | 720×480 BFF, intra VLC table 1, non-linear q, 10-bit DC | 2 | 0.38 % | 72.3 dB |
| m2v_imx_422_intra | 720×608 4:2:2 intra (IMX / D-10 style) | 1 | 0.23 % | 74.6 dB |
| m2v_hd422_longgop | 1920×1080 4:2:2 interlaced long GOP (4:2:2@High) | 2 | 0.20 % | 75.1 dB |
| m2v_1080i | 1920×1080 4:2:0 interlaced, 25 Mb/s (Main@High) | 3 | 0.25 % | 74.2 dB |
| m2v_escapes_qmatrix | q = 1 (escape codes), custom intra/inter matrices | 2 | 1.80 % | 65.6 dB |
| m2v_ipp | no B pictures | 2 | 1.67 % | 65.9 dB |
| m1v_sif | MPEG-1 352×240 IBBP | 2 | 0.59 % | 70.4 dB |
| m1v_q1 | MPEG-1 q = 1 (8/16-bit escapes) | 2 | 1.93 % | 65.2 dB |

FFmpeg's encoder never writes field pictures, 16x8 or dual-prime prediction, so
`src/synth_tests.rs` writes random but valid streams with them (random macroblock types,
vectors, field selects, quantiser scales and coefficients; skipped macroblocks; concealment
vectors; 4:2:0 and 4:2:2; TFF and BFF) and FFmpeg decodes the same streams with `-xerror`:

| Stream | Max Δ | PSNR |
|---|---|---|
| field pictures I/P/B + 16x8, TFF 4:2:0 / BFF 4:2:2 | 2 | 65.7–66.0 dB |
| field pictures + dual prime, TFF 4:2:0 / BFF 4:2:2 | 2 | 66.5–67.5 dB |
| interlaced frame pictures + field MC + dual prime, TFF / BFF | 2 | 64.8–65.7 dB |
| frame pictures with B, 4:2:2, alternate scan, non-linear q | 1 | 66.8 dB |
| seed sweep: 8 seeds × 4 configurations (160 streams checked during development) | ≤ 2 | ≥ 64.7 dB |

Residuals are kept realistic (dequantised coefficients within ±300): with extreme residuals
FFmpeg's integer IDCT saturates differently from the exact one.

## Performance

`cargo test --release -p filmcraft-mpeg2v --test it perf:: -- --ignored --nocapture` decodes the
1080i fixture (1920×1080 4:2:0, 25 Mb/s, IBBP, field MC/DCT; FFmpeg-encoded) on an M4 Pro
(14 cores) that was heavily loaded by other jobs (load average ≈ 190), best of five runs:
**271 fps on one thread, 274 fps with slice rows on rayon** (runs under heavier momentary load
fell to 77–150 fps). These are lower bounds.

## Tests

- `src/vlc.rs`: every table builds without prefix clashes; Kraft sums equal 1 minus exactly the
  codes the standard reserves; every coded_block_pattern and motion_code value appears once;
  motion codes are the address-increment codes plus a sign; 111 run/level pairs per DCT table.
- `src/idct.rs`: IEEE 1180; DC-only path equals the full transform for every DC value.
- `src/headers.rs`, `src/mc.rs`, `src/bits.rs`: scans, start codes, frame rates, half-sample
  rounding, field addressing, edge clamping.
- `src/tests.rs`: a hand-built intra picture decodes to its DC; truncation at every byte and
  3000 random mutations never panic.
- `src/synth_tests.rs`, `tests/it/oracle.rs`: see above (single-threaded decode identical to the
  threaded one).
