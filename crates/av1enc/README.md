# filmcraft-av1enc

Clean-room pure-Rust baseline AV1 software encoder according to the AOMedia AV1 Bitstream &
Decoding Process Specification v1.0.0 with Errata 1.

## Conformance and Architecture

- **Specification**: AOMedia AV1 Specification v1.0.0 with Errata 1.
- **Pure safe Rust**: `#![deny(unsafe_code)]` with zero panics, no `unwrap` or `expect` in production code paths, returning `Result<T, Error>`.
- **Clean-room**: Pure arithmetic coding and bitstream synthesis implemented directly from mathematical specifications and official syntax tables without linking, bundling, or copying third-party code.
- **Independent oracle verification**: The generated Open Bitstream Units (Temporal Delimiter, Sequence Header, Frame Header, and Tile Group) are validated and bit-exact decoded by `filmcraft-av1::Decoder`, verified with external FFmpeg / libdav1d oracle decode tests, and packaged by `filmcraft-isobmff` into standard MP4 files.

## Scope & Capabilities

This crate provides a pure-Rust baseline software encoder (Layer 0) designed for fast preview exports, proxies, and offline encodes:

1. **Compliant Bitstream Synthesis**:
   - Temporal Delimiter, Sequence Header, Frame Header, and Tile Group OBUs.
   - Profile 0 (8-bit 4:2:0 chroma subsampling) with configurable resolutions up to 8K.
   - Accurate Level index calculation (`seq_level_idx_0`: Level 4.0 up to 1080p, Level 5.0 for 4K).
2. **Boolean Entropy Coder**:
   - Clean-room Daala-style arithmetic coder (`SymbolEncoder`) operating on 15-bit precision CDF tables (Section 9 of the AV1 spec).
3. **Intra & Inter Prediction Engines**:
   - Intra modes: DC, Vertical, Horizontal, Smooth, and Paeth (Section 7.11.2).
   - Inter modes: P-frames with `LAST_FRAME` reference buffer tracking and `ZERO_MV` motion compensation.
   - Scene cut detection: temporal SAD analysis dynamically triggering Keyframe insertion on scene changes.
   - Chunked SIMD SAD evaluation using `as_chunks::<16>()`.
4. **Recursive Superblock Partitioning**:
   - Quadtree block partitioning from 64×64 down to 8×8 with boundary split syntax for odd and non-block-multiple frame dimensions.
5. **Non-Skip Transform Residual Coding**:
   - 2D DCT transform and quantization with reconstructed frame feedback in encoder reference buffers.
   - Daala-style entropy coding of transform block skip (`write_txb_skip`), EOB position, coefficient base levels, and Exp-Golomb escape codes.
6. **In-Loop Filtering**:
   - CDEF (Constrained Directional Enhancement Filter) signaled in sequence and frame headers.
7. **Rate Control & CRF Mapping**:
   - 1-pass CBR and VBR with sliding bit reservoir.
   - Constant Quality (CRF): FilmCraft's UI CRF range 0..51 is mapped via `(crf * 4.0).round().clamp(0.0, 255.0) as u8` to AV1 8-bit quantization index (`qindex` 0..255).
   - Direct `Qindex` mode and Adaptive Quantization (AQ) based on spatial variance.
8. **Multicore Parallelism**:
   - Superblock processing parallelized via `rayon` behind the optional `threads` feature (with pure sequential fallback).

## Honest Assessment & Limitations

Production AV1 encoders (such as `svt-av1` and `rav1e`) represent hundreds of person-years of optimization. For transparency, current baseline limitations include:

- **Motion Search**: Inter frames currently evaluate zero-motion vectors (`ZERO_MV`); fractional-pel diamond/hexagon block search is planned for future milestones.
- **Bi-directional Prediction**: Currently produces I and P frames without B-frames or compound references.
- **Loop Restoration & Deblocking**: Loop restoration filter syntax is bypassed in the baseline profile.

## Layering

As an L0 codec crate in FilmCraft's architecture, `filmcraft-av1enc` depends only on `filmcraft-bitstream` and standard library mathematical routines (`cargo xtask layers` verified).
