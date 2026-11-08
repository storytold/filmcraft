# filmcraft-av1

Clean-room, pure-Rust (no `unsafe`) AV1 decoder, implemented from the public **AV1 Bitstream &
Decoding Process Specification, version 1.0.0 with Errata 1** (AOMediaCodec/av1-spec, git
commit `5e04f3f`, 2023-06-12). It is an L0 crate: it depends only on `filmcraft-bitstream`,
`thiserror` and (optional, feature `threads`) `rayon`, and builds for `wasm32-unknown-unknown`.

All constant tables (default CDFs, scan orders, quantiser lookups, filter taps, …) and the
named constants are extracted from the specification's Markdown source by
[`tools/extract_tables.py`](tools/extract_tables.py) into `src/spec_tables.rs` (every table's
shape is checked against its declared dimensions; one missing comma in the spec's
`Split_Tx_Size` initialiser is reported and repaired). No code was taken from libaom, dav1d,
SVT-AV1 or any other implementation; ffmpeg (libsvtav1 to encode fixtures, libdav1d to decode the
reference) is used only as an external test oracle.

## Conformance status

| Stage | Spec | Status |
|---|---|---|
| OBU parsing (low-overhead format), temporal delimiters, operating-point dropping | 5.3, 7.5 | done |
| Sequence header, color config, timing / decoder model info | 5.5 | done |
| Uncompressed frame header (all fields, frame size / superres / render size, tile info, quantiser, segmentation, delta q/lf, loop filter, CDEF, LR, tx mode, skip mode, global motion, film grain params) | 5.9 | done |
| Reference frame state: set_frame_refs, setup_past_independence, load_previous, reference update, show_existing_frame | 7.8, 7.20, 7.21 | done |
| Symbol decoder, CDF adaptation, init / load / save / frame-end CDF update | 8.2, 8.3 | done |
| Tiles and tile groups (uniform and explicit spacing, tile size bytes) | 5.11 | done (tiles decoded in parallel) |
| Intra frame mode info: partitions, skip, segment id, CDEF index, delta q / lf, y / uv modes, angle deltas, CfL alphas, palette (colors, cache, color index map), filter intra, tx size (incl. var-tx syntax) | 5.11 | done, bit-exact |
| Coefficients (all_zero, eob, base / br levels, Golomb, dc sign, tx type sets, scans) | 5.11.39 | done, bit-exact |
| Dequantisation incl. quantiser matrices | 7.12 | done |
| Inverse transforms: DCT 4–64, ADST 4/8/16, flip ADST, identity 4–32, WHT (lossless), rectangular | 7.13 | done, bit-exact |
| Intra prediction: DC, V/H + directional with edge filter and upsampling, smooth (3), Paeth, recursive filter intra, CfL, palette | 7.11.2, 7.11.4, 7.11.5 | done, bit-exact |
| Inter mode info: segment id prediction, skip mode, reference frames (single / compound, uni / bi), modes, DRL, MV coding, inter-intra, motion mode, compound type, interpolation filters | 5.11.18–5.11.32 | done, bit-exact |
| MV prediction: spatial / temporal candidate stacks, global MV, extra search, clamping, warp samples | 7.10 | done, bit-exact |
| Motion field estimation (projection) and motion vector storage | 7.9, 7.19 | done, bit-exact |
| Inter prediction: MV scaling (scaled references), 8-tap sub-pixel filters, global and local warp (warp estimation, shear), OBMC, wedge / difference-weighted / inter-intra masks, distance weights, averaging | 7.11.3 | done, bit-exact |
| Intra block copy | 7.11.3 | done, bit-exact |
| Output policy with scalability (highest spatial layer per temporal unit) | 7.18.1 | done, bit-exact |
| Loop filter (all filter sizes, deltas, segment / ref / mode adjustments) | 7.14 | done, bit-exact |
| CDEF | 7.15 | done, bit-exact |
| Super-resolution upscaling | 7.16 | done, bit-exact |
| Loop restoration (Wiener, self-guided, switchable; stripes) | 7.17 | done, bit-exact |
| Film grain synthesis (output pictures only; `Decoder::apply_film_grain` turns it off) | 7.18.3 | done, bit-exact |
| Large-scale tile / tile list OBUs | 7.3 | not planned |

## Accuracy

OBU and tile length regressions (`decoder::tests::hostile_lengths_*`, #2801) use synthetic
inputs, including truncations and bit flips, without ffmpeg. Oversized LEB128 OBU lengths and
four-byte tile lengths return `Error::Truncated` instead of wrapping or truncating on 32-bit
targets. Valid partial tile groups retain their payloads. See
[32-bit parser tests](../../docs/testing.md#32-bit-parser-regressions).

`tests/it/oracle_intra.rs` encodes all-intra streams with ffmpeg's libsvtav1 (with and without the
in-loop filters; 64×64 and 128×128 superblocks; intra edge filter on and off) and compares every
decoded frame with libdav1d: **bit-exact** on all 13 fixtures (8- and 10-bit 4:2:0; 128×128 to
1280×720 including odd sizes; testsrc2, mandelbrot, gradients and noise; presets 1–8).

`tests/it/conformance.rs` downloads libaom's conformance test vectors on first use
(storage.googleapis.com/aom-test-data) and compares with libdav1d: `av1-1-b8-02-allintra`
(39 frames, deblocking + CDEF + self-guided restoration), `05-mv`, `06-mfmv`, `24-monochrome`
and `b10-23-film_grain-50` are **bit-exact** in the default run. The ignored
`conformance_vectors_extended` test covers the wider set; bit-exact today: `01-size-16x16`,
`-66x66`, `-196x196`, `-226x226`, `00-quantizer-00/31/63` (8-bit) and `-00/40` (10-bit),
`04-cdfupdate`, `05-mv`, `06-mfmv`, `22-svc-L1T2`, `22-svc-L2T1`, `22-svc-L2T2` (spatial layers
with scaled inter-layer prediction), `24-monochrome` (8 and 10-bit) and
`16-intra_only-intrabc-extreme-dv` (1080p intra block copy), `23-film_grain-50` (8 and 10-bit).

`tests/it/oracle_inter.rs` also checks draft mode: every unflagged frame stays bit-exact with
libdav1d and only flagged frames differ, with 1 and 4 threads. `tests/it/oracle_inter.rs` encodes SVT-AV1 GOPs (hierarchical references, compound prediction,
OBMC / warped motion, motion-field projection, all loop filters; presets 3–8, 8- and 10-bit, odd
sizes), plus super-resolution (denominator 12) and film-grain streams, and compares every frame
with libdav1d: **bit-exact** on all 6 fixtures.

## Threading and performance

`Decoder::new()` uses all cores (`Decoder::with_threads(1)` decodes on the calling thread; on
wasm32, or without the default `threads` feature, everything runs on the calling thread). Headers are parsed in order; each frame is
then decoded by a frame worker. With worker threads it first reads the symbols of all its tiles,
which needs only the parse results of its references (CDFs, segment ids, saved motion vectors),
publishes its own, and then reconstructs from the recorded blocks. Frames with several tiles
read the symbols of their tiles in parallel (one after another with intra block copy). A frame reconstructs superblock row by superblock
row (that row of every tile), each row waiting only for the reference rows its motion vectors
reach (row wavefront), and the in-loop filters (deblocking, CDEF, loop restoration) follow a few
64-row bands behind and publish each final band for the frames predicting from it
(`decoder.rs` and `postfilter.rs` give the dependency rules). Super-resolution frames wait for
their whole references and filter the whole frame. There is one frame worker per thread (at most
16), and the frames in flight are bounded by their size. Independent frames (hierarchical GOP
layers) decode concurrently. Results never depend on scheduling (the oracle tests compare 1 thread, many threads
and libdav1d). With frame threads, pictures may be returned by a
later `decode_pts` call or by `flush`; each carries the `pts` of the temporal unit that showed it.

`tests/it/perf.rs` (ignored) prints single-threaded fps (thread CPU time), per-stage ms/frame
(`Decoder::stats()`) and all-core fps with its per-stage ms/frame for 1080p fixtures (key frames,
GOPs, tiles, low delay) and a single-tile 2160p GOP; `AV1_PERF_ONLY=a,b` picks fixtures. The hot loops (sub-pixel filters,
CDEF, inverse transforms, deblocking, entropy decoding) are written for auto-vectorisation.

Measured 2026-10-01 on a 14-core Apple Silicon Mac (release build, SVT-AV1 preset 8 1080p
fixtures from `tests/it/perf.rs`), before (`31f8b3d`: hot loops only, no threading) and after tile / frame /
post-filter threading. The machine was heavily loaded by other builds during both runs (load
average 200–400), so the all-core wall figures understate what an idle machine reaches and are
noisy; single-threaded figures use thread CPU time and are comparable.

| Fixture | 1 thread before | 1 thread after | 14 threads before | 14 threads after |
|---|---|---|---|---|
| `perf_1080p_intra` (10 key frames) | 43.3 fps | 43.0 fps | 6.5 fps | 20.8 fps |
| `perf_1080p_gop` (60 frames, crf 30) | 45.5 fps | 40.7 fps | 7.7 fps | 11.0 fps |
| `perf_1080p_gop_10bit` | 41.3 fps | 35.9 fps | 8.0 fps | 6.6 fps |
| `perf_1080p_gop_hq` (crf 18) | 19.5 fps | 19.4 fps | 2.4 fps | 3.6 fps |
| `perf_1080p_gop_tiles` (4x2 tiles) | 44.4 fps | 44.7 fps | 2.7 fps | 16.3 fps |

### M4.10 (2026-10-03)

CPU cycles per frame counted by the kernel (`/usr/bin/time -l`, all threads summed; mean of two
interleaved base / after rounds at load average 190-320) with
`AV1_THREADS=n cargo run --release -p filmcraft-av1 --example av1dec -- in.ivf` on the
`cargo xtask bench` decode clips (SVT-AV1 preset 10, CRF 35, keyint 48, testsrc2 + grain),
bit-exact with libdav1d:

| stream | threads | Mcycles / frame before | after | after, draft mode |
|---|---|---|---|---|
| 1080p (120 frames) | 1 | 60.5 | **48.3** (−20 %) | 46.2 |
| 1080p | 14 | 63.6 | **51.9** (−18 %) | 50.3 |
| 2160p (72 frames) | 1 | 212.5 | **172.4** (−19 %) | 167.9 |
| 2160p | 14 | 220.0 | **177.1** (−20 %) | 171.1 |

What changed: the sub-pixel filter passes run over fixed block widths (taps outer, columns
inner); frame and tile-region planes come from a bounded pool of recycled buffers (zeroed on
reuse) instead of a fresh `calloc` and `munmap` per frame (allocation, page faults and zeroing
were ~10 % of the time); the palette mode-info arrays (40 bytes per 4x4 unit) are only
allocated when screen content tools are allowed; CDF adaptation is branch-free (a branch-free
symbol search was tried and was slower: most symbols have 2-4 values). Single-threaded 4K now
splits into coefficient parsing ~20 %, inter prediction ~18 %, other block-level parsing
~12 %, deblocking ~11 % (still one edge at a time), inverse transforms ~12 %, CDEF ~5 %.

Draft mode (`Decoder::set_draft(true)`, `AV1_DRAFT=1`; reduced-resolution playback only): shown
frames whose `refresh_frame_flags` is 0 — no reference slot keeps them, so neither their samples,
motion vectors nor CDFs reach another frame — skip deblocking, CDEF and loop restoration
(super-resolution still runs, it sets the output size) and are flagged `Picture::draft`. In
SVT-AV1's hierarchical GOPs that is the top layer, half of the frames.

Frame threading on a Ryzen 7800X3D (8 cores / 16 threads, release `examples/av1dec`, median of 7
alternating runs against upstream `6909e461`, 2026-11-08), wall time for the whole file:

| Fixture | Threads | Before | After | Speed-up |
|---|---|---|---|---|
| `perf_1080p_gop` (60 frames) | 1 | 69.2 fps | 69.1 fps | 1.00x |
| `perf_1080p_gop` | 16 | 159.6 fps | 227.3 fps | 1.42x |
| `perf_2160p_gop` (30 frames) | 1 | 17.9 fps | 17.8 fps | 1.00x |
| `perf_2160p_gop` | 16 | 40.2 fps | 57.7 fps | 1.43x |

Peak RSS at 16 threads goes from 278 to 365 MB (1080p) and from 1031 to 1217 MB (2160p, +18%):
every frame in flight holds its parse record and its final samples are kept in 64-row bands.

With one frame worker per thread, the row wavefront for frames with several tiles and parse
waits limited to what parsing reads (same machine, median of 5 alternating runs, 2026-11-08):

| Fixture | Threads | Before (`6909e461`) | After | Speed-up | Peak RSS before / after |
|---|---|---|---|---|---|
| `perf_1080p_gop` | 4 | 95.7 fps | 185.2 fps | 1.94x | 148 / 236 MB |
| `perf_1080p_gop` | 16 | 155.0 fps | 259.7 fps | 1.68x | 274 / 560 MB |
| `perf_2160p_gop` | 4 | 23.6 fps | 47.1 fps | 2.00x | 512 / 798 MB |
| `perf_2160p_gop` | 16 | 39.9 fps | 65.8 fps | 1.65x | 994 / 1353 MB |
| `perf_1080p_gop_tiles` (4x2 tiles) | 16 | 186.3 fps | 237.2 fps | 1.27x | 328 / 617 MB |
| SVT-AV1 2160p, 4x2 tiles (30 frames) | 16 | 57.1 fps | 70.8 fps | 1.24x | 1197 / 1502 MB |

Known limitations of the frame threading (the output is the same in every case, only the
overlap is smaller):

- A frame whose parse record would pass 32 bytes per luma sample of a tile (pathological streams:
  tiny blocks, large palettes, every coefficient coded), or that cannot reserve its record from
  the decoder-wide budget of 1 GiB shared by the frames in flight, is decoded again in one pass.
- A superblock row using local warped motion, global motion beyond translation or a scaled
  reference waits for the whole reference instead of the rows its motion vectors reach.
- Super-resolution frames do not use the row wavefront: they wait for their whole references
  and filter the whole frame.
- The tile columns of a superblock row are reconstructed one after another on the frame's worker
  (their symbols are still read in parallel).

## Limits

Frames beyond the AV1 level 6.x limits (Annex A: at most 16384 wide, 8704 high and 35 651 584 luma samples
in total, so 16384 x 2176 or 8192 x 4352 at most) are refused with `Error::Invalid("frame size")`. The sequence
header can code up to 65536 x 65536, which would ask for gigabytes of buffers from a few bytes of hostile input;
no real stream comes close to level 6.3, the highest defined level.

## API

```rust
let mut dec = filmcraft_av1::Decoder::new();
for sample in samples {                  // one temporal unit (MP4 / Matroska sample) each
    for pic in dec.decode_pts(&sample, pts)? {   // shown frames ready so far, planar u16
        // pic.width, pic.height, pic.bit_depth, pic.planes[0..3], pic.pts
    }
}
let rest = dec.flush();                  // pictures still in flight at the end
```
