# filmcraft-hevc

Clean-room, pure-Rust (no `unsafe`) H.265 / HEVC decoder, implemented from the public ITU-T
Rec. H.265 (ISO/IEC 23008-2) specification. It is an L0 crate: it depends only on
`filmcraft-bitstream`, `thiserror`, and optionally `rayon` (default feature `threads`), and builds
for `wasm32-unknown-unknown`.

All syntax tables (the CABAC `initValue` tables arranged per `initType` following Table 9-4, default
scaling lists, `intraPredAngle` / `invAngle`, the chroma QP table, the deblocking beta'/tC' table,
the luma / chroma interpolation filter taps and the 32x32 DCT matrix) were extracted from the
specification text by [`tools/extract_tables.py`](tools/extract_tables.py) (it only reads the text
of the ITU-T PDF, see the script header); the output is `src/spec_tables.rs`. Scan orders are
derived by the algorithms of 6.5.3-6.5.5. No code was taken or ported from FFmpeg, libde265, x265 or
the HM reference software; ffmpeg / libx265 / VideoToolbox are only used as external test oracles
and fixture generators.

## Status

| Area | Status |
|---|---|
| NAL / Annex-B / `hvcC` length-prefixed input | done |
| VPS, SPS (VUI, HRD skipped, scaling lists, st/lt RPS, PCM), PPS (tiles, WPP, deblocking control, scaling lists, list modification) | done |
| Slice segment header (all fields: st/lt RPS, list modification, pred weight table, entry points, header extension) | done |
| POC, RPS marking, missing-reference generation, RefPicList construction, DPB output / bumping (C.5.2) | done |
| CABAC: all Main / Main 10 syntax elements, WPP context sync, dependent slice context restore, tiles | done |
| Coding quadtree, CU (skip / intra / inter, AMP, PCM, transquant bypass), PU, transform tree | done |
| Intra: 35 modes, reference substitution, [1 2 1] filtering, strong intra smoothing, constrained intra | done |
| Inter: merge (spatial, temporal, combined bi-pred, zero), AMVP, TMVP, 8-tap / 4-tap interpolation, default and explicit weighted prediction | done |
| Scaling (flat and scaling lists), DCT 4..32, DST 4x4, transform skip, sign data hiding | done |
| Deblocking (bS, slice / tile boundary rules, PCM / bypass exemption), SAO (band / edge, boundary rules) | done |
| Frame-level multithreading (CTB-row granularity dependencies) | done |
| Draft mode (non-reference pictures skip deblocking / SAO; reduced-resolution playback only) | done |
| Tiles / WPP | parsed and decoded sequentially within a picture (parallelism is across pictures) |
| 4:2:2 (Main 4:2:2 10 / RExt profile): chroma geometry, Table 8-3 intra mode mapping, stacked chroma transform blocks with their second cbf flags, chroma QP = Min(qPi, 51), chroma motion vectors, deblocking / SAO geometry; 8–12-bit | done, bit-exact on libx265 fixtures and a Fujifilm X-S20 clip |
| 4:4:4 / monochrome, range-extension coding tools (RDPCM, cross-component prediction, extended precision, persistent Rice adaptation, bypass alignment, chroma QP offset lists) | not yet (`Error::Unsupported`) |
| SCC, multilayer (SHVC / MV-HEVC), 3D extensions | not supported (layers > 0 are ignored) |

## API

```rust
use filmcraft_hevc::{ChromaFormat, Decoder, Plane};

let mut dec = Decoder::new(); // or Decoder::with_threads(n), Decoder::from_hvcc(&hvcc)?
for (pts, access_unit) in access_units {
    for pic in dec.decode(access_unit, pts)? {
        // pic.width / pic.height (cropped), pic.bit_depth, pic.chroma (ChromaFormat::C420 / C422),
        // pic.y / pic.u / pic.v: Plane::U8(Vec<u8>) for 8-bit, Plane::U16(Vec<u16>) above,
        // pic.y_stride / pic.uv_stride, pic.pts, pic.poc, pic.key (IRAP), pic.color (VUI), pic.sar
    }
}
for pic in dec.flush() { /* ... */ }
if let Some(err) = dec.take_error() { /* error reported by a decoding thread */ }
// dec.set_draft(true): draft mode (playback at reduced resolution only)
```

`decode` expects whole access units (a buffer with several complete access units — e.g. an entire
Annex-B file — also works). RASL pictures following a CRA that starts the stream are skipped, as
ffmpeg does. `Decoder::stats()` returns counters of what was decoded (slice types, IDR/CRA, tiles,
WPP, weighted prediction, TMVP, SAO, block kinds...).

### Architecture

Same design as `filmcraft-h264`: the calling thread parses parameter sets and slice headers and does
POC / RPS / DPB bookkeeping; the CTU layer of each picture runs as a job (rayon pool). A job
reconstructs into private planes, deblocks CTB row r once row r+1 is decoded, applies SAO to row r
once row r+1 is deblocked, and publishes final rows (samples + 16x16 compressed motion for TMVP)
into the shared `Frame` (`OnceLock` per CTB row). Motion compensation of later pictures blocks per
row on exactly the reference rows it reads. Samples are stored as `u8` for 8-bit pictures (half
the memory traffic in prediction reads, in-loop filters and output) and as `u16` above 8 bits: the
slice decoder and the filters are generic over the sample type (`picture::Sample`), monomorphised
once per type.

Draft mode (`Decoder::set_draft(true)`; FilmCraft enables it with Settings ▸ Playback ▸ Draft
decoding while the Program monitor plays at 1/2 or 1/4): sub-layer non-reference pictures
(TRAIL_N, RASL_N, ...) of the highest temporal sub-layer — no later picture predicts from them,
and TMVP reads only reference pictures — skip deblocking and SAO and are flagged
`Picture::draft`; every other picture is unchanged. libx265's non-reference B pictures are such
pictures.

Modules: `params` (VPS/SPS/PPS/tile layout), `slice` (NAL + slice header), `dpb` (POC, RPS, lists,
bumping), `cabac`, `slicedec` (CTU syntax + reconstruction), `mvpred` (merge / AMVP / TMVP),
`intra`, `inter`, `transform`, `filter` (deblocking, SAO, row publication).

## Tests

`cargo test -p filmcraft-hevc` (fixtures are generated on first use into `target/fixtures/hevc/`
with ffmpeg; tests print a message and skip when ffmpeg is absent).

- Unit tests: CABAC engine round trip against an encoder model and context initialisation, scan
  orders, DCT matrix properties, QpC, intra predictors, transform DC / bounded-region shortcuts,
  MV scaling.
- Synthetic streams (`src/synth_tests.rs`): a small CABAC encoder writes streams with what libx265
  never produces — tiles (uniform and explicit spacing, loop filter across tiles on / off), PCM
  (with and without in-loop filtering), multiple slices, dependent slice segments (starting inside
  and at the start of a tile), long-term reference pictures, reference list modification. Expected
  output is known exactly where filters leave PCM samples alone; every stream is also compared with
  ffmpeg.
- Robustness: hvcC input with pts round trip; randomly corrupted, truncated and garbage input never
  panics or deadlocks (single- and multi-threaded).
- Conformance (`tests/conformance.rs`): every fixture is decoded with 1 thread, 3 threads and every
  core and compared sample-exactly with `ffmpeg -f rawvideo -pix_fmt yuv420p / yuv420p10le /
  yuv422p / yuv422p10le`; on mismatch
  the first differing frame, plane, sample, CTB and 8x8 block are reported. Output pts/POC order is
  checked too, and that no picture is flagged draft without draft mode. `compare_detects_mismatch`
  guards the harness itself. Draft mode (`draft_mode_changes_only_flagged_non_reference_pictures`):
  on B-pyramid / weighted fixtures, single- and frame-threaded, every unflagged picture is
  bit-exact and only flagged pictures differ.

### Fixture matrix (all bit-exact, single- and multi-threaded)

libx265 unless noted (x265 enables WPP by default, so most streams carry entry points).

| fixture | source / size | configuration |
|---|---|---|
| intra_main / intra_main10 | testsrc2+noise 352x288 | keyint=1, 8 / 10-bit |
| intra_nosao_nodbk | testsrc2+noise 352x288 | intra, no SAO, no deblocking |
| ultrafast / veryfast / medium / slow | 352x288 | presets (slow: rect + AMP) |
| medium_main10 / slow_main10 | 352x288 | Main 10 presets |
| p_only | 352x288 | bframes=0 |
| bframes | 352x288, 30 frames | bframes=8, b-pyramid, ref=4 |
| no_sao / no_deblock / deblock_m3_3 | 352x288 | loop filter variants |
| no_wpp | 352x288 | WPP off |
| rect_amp / min_cu16 | mandelbrot / testsrc2 | rect + AMP, min CU 16 |
| tskip | 352x288 | transform skip |
| weightp / weightp_main10 | fade-in | weightp + weightb |
| crf0 / crf51 / qp1_main10 | | quantizer extremes |
| lossless / cu_lossless | 176x144 | cu_transquant_bypass |
| slices4 | 352x288 | 4 slices per picture |
| scaling_list | 352x288 | default scaling lists |
| ctu16 / ctu32_tu4 | 352x288 | CTB 16, CTB 32 with deep TU trees, max TU 16 |
| no_signhide / constrained_intra / no_tmvp / no_strong_intra / max_merge1 | 352x288 | tool toggles |
| open_gop | 352x288, 40 frames | CRA + RASL / RADL, keyint 12 |
| keyint5_idr | 352x288 | closed GOP, IDR every 5 |
| odd_1918x1078 | 1918x1078 | conformance-window cropping |
| hd_1080p | 1920x1080 | preset fast |
| uhd_4k_main10 | 3840x2160 | Main 10 ultrafast |
| vt_main / vt_main10 | 640x360 | Apple VideoToolbox (hevc_videotoolbox), macOS only |
| intra_422 / intra_422_10 / intra_422_10_nosao_nodbk | 352x288 | 4:2:2 8 / 10-bit intra, with and without the in-loop filters |
| medium_422_10 / slow_422 / p_only_422_10 / bframes_422_10 | 352x288 | 4:2:2 presets, P only, B pyramid |
| no_sao_422_10 / deblock_m3_3_422_10 / rect_amp_422_10 / tskip_422_10 / weightp_422_10 | 352x288 | 4:2:2 tool toggles |
| qp_422_10 / crf51_422_10 / lossless_422_10 | 352x288 / 176x144 | 4:2:2 adaptive QP (cu_qp_delta), quantizer extremes, lossless |
| ctu32_tu4_422_10 / ctu16_422_10 / scaling_list_422_10 / constrained_intra_422_10 / slices4_422_10 | 352x288 | 4:2:2 block structure, scaling lists, constrained intra, slices |
| odd_1918x1078_422_10 / uhd_4k_422_10 | 1918x1078 / 3840x2160 | 4:2:2 cropping and 4K |
| synthetic (7 streams) | 96x64 | tiles, PCM, slices, dependent segments, LTR, list modification |

`cargo test -p filmcraft-hevc --test conformance coverage_report -- --ignored --nocapture` prints the
decoder statistics per fixture.

## Performance

`cargo test --release -p filmcraft-hevc --test perf -- --ignored --nocapture` (bit-exactness is
verified first). Apple M4 Pro (14 cores), measured while the machine was heavily shared (load
average 50-100), so these are lower bounds:

| stream | 1 thread | 14 threads |
|---|---|---|
| 1080p Main, x265 medium CRF 24, 60 frames | ~75 fps | ~225 fps |
| 2160p Main 10, x265 fast CRF 26, 20 frames | ~20 fps | ~95 fps |

No intrinsics: the hot loops are written to auto-vectorise. M4.8
profiling (`cargo xtask bench`, macOS `sample`) found the inverse transform at over half of the
decoder's CPU time: it evaluated the full matrix product with strided table reads. It now sums
contiguous basis rows scaled by the non-zero coefficients only, with the block size a compile-time
constant so the loops vectorise (exact integer sums, so bit-exact; `transform::tests` checks it
against the direct product on random sparse and dense blocks). SAO edge offset has a check-free
loop for samples whose neighbours are inside the CTB. Together: ~60 % less CPU per frame
(1080p 78 → 33 ms, 2160p 320 → 129 ms of CPU per frame through the media stack, alternating A/B), see
[docs/performance.md](../../docs/performance.md).
`cargo run --release -p filmcraft-hevc --example hevcdec -- in.hevc out.yuv` decodes a file
(`HEVC_THREADS=n`, `HEVC_LOOPS=k`, `HEVC_DRAFT=1`).

M4.10 (2026-10-03), CPU cycles per frame counted by the kernel (`/usr/bin/time -l`, all threads
summed; mean of two interleaved base / after rounds at load average 190-320) on the
`cargo xtask bench` decode clips (x265 preset fast, CRF 22, testsrc2 + grain), bit-exact:

| stream | threads | Mcycles / frame before | after | after, draft mode |
|---|---|---|---|---|
| 1080p (120 frames) | 1 | 90.9 | **67.3** (−26 %) | 61.2 |
| 1080p | 14 | 100.2 | **75.1** (−25 %) | 67.3 |
| 2160p (72 frames) | 1 | 362.6 | **262.3** (−28 %) | 232.5 |
| 2160p | 14 | 398.3 | **284.8** (−28 %) | 260.1 |

What changed: motion compensation zeroed two 10 KB windows per prediction block per component
and list (11 % of the time in `memset`) and filtered with dynamic-width scalar sums; it now keeps
reusable scratch windows and runs taps outer / outputs inner over fixed block widths. SAO
looked its offset up per sample in a table (20 % of the time); the edge / band offset is now
selected per lane from the neighbour-sign sum / band index, so the runs vectorise. Uni / bi
output clips with min / max instead of `clamp` (whose bound assert kept it scalar). Runs of
bypass bins (signs, Rice suffixes, SAO / intra fields) decode with one division, and
context-coded bins are branch-free. Single-threaded 4K now splits into CABAC residual parsing
~28 %, reference window copies ~11 %, interpolation ~11 %, deblocking + SAO + row publication
~15 %, inverse transform ~8 %.

## Known gaps

- 4:4:4 and monochrome streams, and the range extension coding tools (implicit / explicit RDPCM,
  cross-component prediction, extended precision, persistent Rice adaptation, bypass alignment,
  chroma QP offset lists) are rejected with `Error::Unsupported`. 4:2:2 streams that use none of
  those tools (what cameras and libx265 write) decode.
- Tiles and WPP substreams are decoded sequentially (entry points are parsed but the decoder finds
  substream starts itself); parallelism is at the picture level.
- Error concealment is minimal: missing references are replaced by grey pictures, undecodable
  slices leave grey / partial content.
- No fixture covers the `pic_output_flag = 0` path or decoding-order-only CRA handling after EOS
  beyond the synthetic streams; libx265 never emits PCM, tiles or long-term references (covered by
  the synthetic streams only).
