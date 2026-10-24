# filmcraft-apv

Clean-room Advanced Professional Video (APV) decoder and encoder in pure Rust, implemented strictly
from the public **IETF RFC 9924** specification (*Advanced Professional Video*, February 2026,
ISSN 2070-1721).

Layer L0: depends only on `filmcraft-bitstream`, `thiserror`, and (optional, feature `threads`,
default on) `rayon`. Builds for `wasm32-unknown-unknown`; `unsafe_code = "forbid"`.

## API

```rust
use filmcraft_apv::*;

// Decode one APV Access Unit (or raw bitstream AU prefixed with a 32-bit au_size).
let frame: Frame = decode_frame(&au)?;
let f8 = decode_frame_with(&au, &DecodeOptions { bit_depth: Some(8), threads: true })?;
decode_frame_into(&au, &DecodeOptions::default(), &mut frame_buf)?;
let hdr: FrameHeader = probe(&au)?;

// Split a multi-AU raw .apv bitstream (RFC 9924 Appendix A).
let aus: Vec<&[u8]> = split_raw_bitstream(&raw_apv_bytes)?;

// Encode progressive intra frames.
let mut enc = Encoder::new(Profile::P422_10, 1920, 1080)?;
let au: Vec<u8> = enc.encode(&frame)?;          // Access Unit starting with 'aPv1'
let raw_au: Vec<u8> = enc.encode_raw_au(&frame)?; // Prefixed with 32-bit au_size (ISOBMFF apv1 / .apv)
let apvc: Vec<u8> = enc.decoder_config_record();  // APVDecoderConfigurationRecord payload
```

## Specification Coverage (IETF RFC 9924)

### Decoder
- **Access Units & PBUs (§5.3.1–5.3.3, Appendix A)**: Accepts both unprefixed Access Units
  (`'aPv1'` signature) and length-prefixed `raw_bitstream_access_unit()` packets. Parses primary
  (`pbu_type == 1`), non-primary (`2`), preview (`25`), auxiliary alpha (`27`), AU info (`65`),
  metadata (`66`), and filler (`67`) PBUs. PBUs or frame headers with non-zero `reserved_zero_*`
  fields are ignored per §5.3.3 and §5.3.5.
- **Profiles & Sampling (§9.3)**: All 7 standard profiles (`422-10` idc 33, `422-12` idc 44,
  `444-10` idc 55, `444-12` idc 66, `4444-10` idc 77, `4444-12` idc 88, `400-10` idc 99) at
  coded bit depths 10–16, with optional output depth rescaling (`8..=16`).
- **Quantization Matrices & Tiles (§5.3.7–5.3.8, §5.3.12–5.3.14)**: Default flat (`16`) and
  custom per-component 8×8 quantization matrices (`use_q_matrix`), optional `tile_size_in_fh`
  verification, and independent per-tile, per-component byte-aligned entropy partitions. Tiles
  decode in parallel via `rayon` when `threads` is enabled.
- **Entropy Decoding (§5.3.15–5.3.16, §7.1)**: Adaptive Golomb-Rice / Exp-Golomb VLC decoding for
  `abs_dc_coeff_diff`, `coeff_zero_run`, and `abs_ac_coeff_minus1` with state tracking (`PrevDC`,
  `PrevDcDiff`, `Prev1stAcLevel`, `PrevLevel`, `PrevRun`) across the 8×8 blocks of each macroblock.
- **Scaling & Inverse Transform (§6.3)**: Exact integer dequantization (`levelScale = [40, 45, 51,
  57, 64, 71]`, `bdShift = BitDepth - 2`) and 2-D separable 8×8 integer inverse transform using the
  RFC 9924 Figure 25 `transMatrix`. Note: in RFC 9924 §6.3.2.2, `transMatrix[m][n]` has row `m` as
  the frequency index (row 0 is `[64; 8]`) and column `n` as the spatial sample index, so the 1-D
  inverse transform evaluates `sum_j transMatrix[j][i] * x[j]`.
- **Metadata (§8)**: Parses `metadata_mdcv` (`payloadType == 5`, Mastering Display Color Volume)
  and `metadata_cll` (`payloadType == 6`, Content Light-Level Information).
- **Robustness (§10)**: All size fields, tile coordinates, VLC codeword lengths, and coefficient
  ranges (`-32768..=32767`) are checked before allocation or shift. Verified by `tests/it/fuzz.rs`
  across thousands of mutated and random inputs.

### Encoder
- Progressive intra encoding for all 7 profiles (`422-10`, `422-12`, `444-10`, `444-12`,
  `4444-10`, `4444-12`, `400-10`).
- Supports fixed `qp` or binary-search frame-budget rate control over forward-transformed tiles.
- Emits conforming tile grids (`tile_width_in_mbs >= 16`, `tile_height_in_mbs >= 8`,
  `TileCols, TileRows <= 20`), optional custom quantization matrices, optional `MDCV`/`CLL`
  metadata PBUs, and `APVDecoderConfigurationRecord` (`apvC`) metadata.

## Accuracy (vs FFmpeg as an External Oracle)

`tests/it/oracle.rs` encodes synthetic test patterns across all 7 profiles, odd dimensions, multiple
tiles, custom asymmetric quantization matrices, and HDR metadata PBUs, then decodes the resulting
`.apv` streams with `ffmpeg -v error -xerror -f apv` and compares against `filmcraft_apv::decode_frame`.
Because APV specifies an exact integer inverse transform, **`filmcraft-apv` is 100% bit-exact
(`max_diff = 0`) with FFmpeg's APV decoder across every profile and test case.**
