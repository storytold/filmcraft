# filmcraft-av1enc

Clean-room pure-Rust baseline AV1 software encoder according to the AOMedia AV1 Bitstream &
Decoding Process Specification v1.0.0 with Errata 1.

## Conformance and Architecture

- **Specification**: AOMedia AV1 Specification v1.0.0 with Errata 1.
- **Pure safe Rust**: `#![deny(unsafe_code)]` with zero panics, no `unwrap` or `expect` in production code paths, returning `Result<T, Error>`.
- **Clean-room**: Entropy coding and bitstream synthesis implemented from public specifications and official syntax tables without linking, bundling, or reading third-party encoder code.
- **Independent oracle verification**: Generated OBUs (Temporal Delimiter, Sequence Header, Frame Header, and Tile Group) are decoded by `craft_video::av1::decode::Decoder`, verified against external `ffmpeg` (`libaom-av1` and `libdav1d`) decoders, and muxed by `filmcraft-isobmff` into standard MP4 files.

## Scope & Capabilities

This crate provides a pure-Rust baseline software encoder (Layer 0):

1. **Compliant Single-Tile Bitstream Synthesis (up to 4K Level 5.0)**:
   - Temporal Delimiter, Sequence Header, Frame Header, and single-tile Tile Group OBUs.
   - Profile 0 (8-bit 4:2:0) bounded by single-tile Level 5.0 limits (`width <= 4096` / `sb_cols <= 64`, `height <= 4352`, `sb_cols * sb_rows <= 2304`, and `width * height <= 8,912,896`).
   - Level selection (`seq_level_idx_0`): Level 4.0 (`8`) up to 1920x1080, Level 5.0 (`12`) above 1080p within the single-tile 4K bounds.
2. **Range Entropy Coder**:
   - Daala-style arithmetic coder (`SymbolEncoder`) operating on 15-bit precision CDF tables (AV1 Spec §8.2 / §9) with Spec §8.2.4 `exit_symbol` trailing-bit termination.
3. **Intra & Inter Prediction**:
   - Luma intra modes: `DC_PRED`, `V_PRED`, and `H_PRED`.
   - Chroma intra mode: `DC_PRED`.
   - Inter mode: P-frames referencing `LAST_FRAME` with `ZERO_MV` prediction when block SAD against reference is within threshold, using AV1 Spec §7.10.2 `find_mv_stack` context derivation (`compute_new_mv_ctx`).
   - Scene cut detection via subsampled temporal SAD, and chunked SAD evaluation (`as_chunks::<16>()`).
4. **Recursive Superblock Partitioning**:
   - Quadtree block partitioning from `64x64` down to `8x8` at `qindex >= 70` and down to `4x4` (`bsl = 0`, shared `4x4` chroma on bottom-right sub-block per AV1 Spec §5.11.25) when `qindex < 70`, with boundary split syntax for non-block-multiple dimensions.
5. **DC-Only Transform Residual Coding**:
   - Computes per-transform-block DC residual relative to reconstructed neighbors, quantizes a single DC level (`dc_level`), and reconstructs using AV1 Spec §7.12.3 + §7.13.3 2D inverse DCT shift (`av1_dc_recon_shift`).
   - Codes transform block skip (`txb_skip`) and the single DC coefficient (`coeff::encode_dc_residual`); AC coefficients are not coded (baseline DC-only coding).
6. **In-Loop Filtering Disabled**:
   - Loop filter (`level = 0`) and CDEF (`strength = 0`) are explicitly signaled disabled in frame headers so encoder and decoder reference buffers stay identical.
7. **Rate Control & CRF Mapping**:
   - 1-pass CBR and VBR with sliding bit reservoir.
   - Constant Quality (CRF): range `0.0..=51.0` mapped via `(crf * 4.0).round().clamp(1.0, 255.0) as u8` to DCT `qindex 1..=255`.
   - Direct `Qindex` mode (`1..=255`; `0` clamped to `1`) and frame-variance Spatial AQ (`apply_spatial_aq`, clamped to `>= 1` and never raising low requested `qindex`).
8. **Multicore Row Analysis**:
   - Superblock mode/split analysis parallelized via `rayon` behind the optional `threads` feature (with sequential fallback).

## Honest Assessment & Limitations

- **DC-Only Residuals**: Without AC residual coding, fine high-frequency detail within textured blocks relies on quadtree partitioning down to `4x4` and DC levels rather than full 2D AC blocks, resulting in substantially lower compression efficiency than full AV1 encoders (`libaom-av1`, `svt-av1`, `rav1e`) and FilmCraft's built-in H.264 encoder.
- **Zero Motion Search**: Inter frames use `ZERO_MV` on static regions only; moving content falls back to intra blocks.
- **Frame Types & Tools**: Produces I and P frames only (no B-frames, compound prediction, CDEF filtering, loop deblocking, or loop restoration).
- **Resolution Limit**: Single-tile encoding enforces AV1 single-tile and Level 5.0 bounds (`max 4096px wide`, `max 2304 superblocks`, `max 8,912,896 pixels`).

## Layering

As an L0 codec crate in FilmCraft's architecture, `filmcraft-av1enc` depends only on `filmcraft-bitstream` and standard library routines (`cargo xtask layers` verified).
