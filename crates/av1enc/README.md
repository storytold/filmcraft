# filmcraft-av1enc

Clean-room pure-Rust baseline AV1 software encoder according to the AOMedia AV1 Bitstream &
Decoding Process Specification v1.0.0 with Errata 1.

## Conformance and Architecture

- **Specification**: AOMedia AV1 Specification v1.0.0 with Errata 1.
- **Pure safe Rust**: `#![deny(unsafe_code)]` with zero panics, no `unwrap` or `expect` in production code paths, returning `Result<T, Error>`.
- **Clean-room**: Pure arithmetic coding and bitstream synthesis implemented directly from mathematical specifications and official syntax tables without linking, bundling, or copying third-party code.
- **Verified decoding**: The generated Open Bitstream Units (Temporal Delimiter, Sequence Header, Frame Header, and Tile Group) are validated and bit-exact decoded by `filmcraft-av1::Decoder` and packaged by `filmcraft-isobmff` into standard MP4 files.

## Current Scope (v0.1.0 Baseline All-Intra)

This crate provides a pure-Rust, zero-dependency (L0 layer) baseline encoder designed for rapid Intra-frame exports and proxy encoding:

1. **Compliant Bitstream Synthesis**:
   - Temporal Delimiter, Sequence Header, Frame Header, and Tile Group OBUs.
   - Profile 0 (8-bit 4:2:0 chroma subsampling) with configurable resolutions up to 8K.
2. **Boolean Entropy Coder**:
   - Clean-room Daala-style arithmetic coder (`SymbolEncoder`) operating on 15-bit precision CDF tables (Section 9 of the AV1 spec).
3. **Intra Prediction Engine**:
   - 5 prediction modes: DC, Vertical, Horizontal, Smooth, and Paeth (Section 7.11.2).
   - Chunked SIMD SAD evaluation using `as_chunks::<16>()`.
   - Precalculated horizontal weights and loop-invariant hoisting for Smooth and Paeth distortion calculations.
   - Early Mode Termination: bypasses complex directional evaluations for near-flat blocks to minimize encode latency.
4. **Multicore Parallelism**:
   - Superblock row analysis and mode selection run concurrently across all available CPU cores using `rayon`.
5. **Rate Control**:
   - 1-pass CBR and VBR with sliding bit reservoir.
   - Constant Quality (CRF mapped to qindex 0..255) and direct Qindex modes.
   - Adaptive Quantization (AQ) based on spatial variance.
6. **Quality Evaluation**:
   - Built-in `compute_mse`, `compute_psnr`, and `compute_psnr_u16` metric helpers for test verification.

## Honest Assessment & Limitations

Production AV1 encoders (such as `svt-av1` and `rav1e`) represent hundreds of person-years of research and millions of lines of code. For clarity and transparent engineering, the current baseline scope has the following limitations:

- **All-Intra Only**: Every frame is currently encoded as a self-contained Intra keyframe. Inter-frame compression (motion search, 1/8-pel motion vectors, and reference frame buffers) is planned for future milestones.
- **Fixed Superblock Partition**: Superblocks are currently encoded with `PARTITION_NONE` (64x64). Recursive sub-block splitting (32x32, 16x16, 8x8, 4x4) is not yet enabled in the bitstream syntax.
- **Skip Residual Coding**: Superblocks currently signal `skip = 1` in the tile payload, relying on directional and smooth intra prediction for reconstruction. Full non-skip transform residual entropy coding (DCT/ADST coefficients, EOB, level/sign bits) is under development in `transform.rs`.
- **In-Loop Filters**: Deblocking, CDEF (Constrained Directional Enhancement Filter), and Loop Restoration are disabled in the frame header.

## Layering

As an L0 codec crate in FilmCraft's architecture, `filmcraft-av1enc` depends only on `filmcraft-bitstream` and standard library mathematical routines (`cargo xtask layers` verified).
