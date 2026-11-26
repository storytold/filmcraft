# filmcraft-insta360

A clean-room reader for the Insta360 INSV metadata trailer, written from camera files
observed with hex tooling and the published `insv-conversion` / `insvtools` descriptions of
the trailer layout. Layer L0: no dependencies beyond `std`, no `unsafe`, builds for
`wasm32-unknown-unknown`. No Insta360 binary was disassembled (AGENTS.md §2); ffmpeg and
ffprobe are used only as external **test oracles**.

## What an INSV file is

An `.insv` (Insta360 X-series and similar) is an ordinary MP4 — `ftyp`/`mdat`/`moov`,
usually two HEVC fisheye video tracks (one per lens) plus AAC audio — followed by a
proprietary *trailer*:

| Part | |
|---|---|
| Trailer header | 72 bytes at end-of-file: `i32` metadata size at +32, `i32` version (3) at +36, the 32-byte ASCII signature `8db42d694ccc418790edff439fe026bf` at +40 |
| Frame chain | metadata frames packed back-to-back in front of the header, each ending with a 6-byte footer: `u8` version, `u8` type, `i32` payload size (the payload lies before the footer) |
| INFO frame (type 1, version 1) | a protobuf: field 1 serial, 2 model, 3 firmware, 22 colour profile, 26 first part of a spanned recording, 54 `OffsetV3` |
| INDEX frame (type 0) | entries `[u8 type][u8 ver][i32 size][i32 offset]`, offsets relative to the trailer region's start — the fallback when the chain is damaged |

`OffsetV3` is `2_` followed by 19 `_`-separated tokens per lens: `xi fx fy cx cy yaw pitch
roll` (degrees), three ignored, `k1 k2 k3 p1 p2` (Brown distortion), `sensor_w sensor_h`,
two ignored. That is the factory MEI (unified) calibration of the two fisheye lenses, in
full-sensor pixels with the circles side by side; `Lens::scale_to_circle` re-expresses it
for the decoded video.

## API

```rust
use filmcraft_insta360::{parse, sniff};

if sniff(&bytes) {
    if let Some(meta) = parse(&bytes)? {
        println!("{:?} {:?}", meta.model, meta.lenses.map(|[a, _]| a.xi));
    }
}
```

`filmcraft-codecs` maps the two video tracks onto decoders and stitches them to
equirectangular with the calibration (`InsvSource`, `stitch`), or stacks them side by side when
a file has no usable calibration.

## Tests

`cargo test -p filmcraft-insta360`: hand-built trailers for the chain walk, the INDEX
fallback, unknown versions, damaged `OffsetV3`, plus every single-byte mutation and every
truncation of a good file under `catch_unwind` (no panics, no wild allocations). Real
camera footage is checked by `cargo test -p filmcraft-codecs --test insta360_insv` when
`FILMCRAFT_INSV` points at an `.insv`.
