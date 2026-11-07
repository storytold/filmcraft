# filmcraft-mpeg4v

Clean-room MPEG-4 Part 2 video decoder in pure Rust: the video of DivX and Xvid files. Layer L0:
depends only on `thiserror`. Builds for `wasm32-unknown-unknown`; no `unsafe`, no threads. No
source of FFmpeg, Xvid or any other decoder was consulted; FFmpeg is used only as an external
fixture generator and test oracle.

## Specifications

| Document | Edition | Used for |
|---|---|---|
| ISO/IEC 14496-2 | 2004 (third edition) | headers (§6), VLC tables (Annex B), inverse quantisation and AC/DC prediction (§7.4), motion vectors and compensation (§7.6), direct mode, video packets and data partitioning |
| IEEE Std 1180 | 1990 | IDCT accuracy test procedure (run as a unit test) |

The tables were written from knowledge of the standard. Each is checked: the coefficient tables
are complete prefix codes whose limits are those of Tables B-18 to B-21, and each of their 204
entries was exercised against FFmpeg's reading (see Accuracy).

## API

```rust
let mut dec = filmcraft_mpeg4v::Decoder::new();
dec.configure(&extradata)?;                       // headers kept outside the stream (AVI strf), optional
for (pts, chunk) in chunks {
    for pic in dec.decode(chunk, pts)? {          // display order
        // pic.y / pic.cb / pic.cr: tight 8-bit 4:2:0 planes; pic.vop_type, pic.pts, pic.info
    }
}
let rest = dec.flush();
let info = filmcraft_mpeg4v::probe(&chunk);       // the layer header: size, aspect, what is missing
let types = filmcraft_mpeg4v::vop_types(&chunk);  // VOP types without decoding
```

`decode` takes one container chunk, whatever VOPs and headers it holds. An anchor is held until the
next anchor, so B-VOPs come out before it. `reset` (a seek) forgets pictures and keeps the headers:
key frames need not repeat them. A B-VOP without its two references (the open GOP's leading
pictures after a seek) and a P-VOP without one are left out.

The packed bitstream of DivX and Xvid (an anchor and a B-VOP in one chunk, a not-coded VOP in the
next slot) needs no setting: two coded VOPs in a chunk show it, the B-VOP takes the chunk's pts and
the anchor that of the not-coded VOP, so one picture comes out per slot.

## Decoder

- Simple Profile: I and P VOPs, one and four vectors, H.263 and MPEG quantisation, custom matrices
  with mismatch control, AC/DC prediction with the three scans, video packets with header
  extension codes, data partitioning, not-coded VOPs, half-sample motion compensation with
  unrestricted vectors and rounding control, both intra DC codings (`intra_dc_vlc_thr`).
- Advanced Simple Profile: B-VOPs with all four modes, direct mode with deltas, macroblocks not
  coded in the anchor after them, quarter-sample motion compensation.
- Non-multiple-of-16 sizes, cropped on output; a layer that changes size mid-stream.
- IDCT: the separable 2-D IDCT in `f32`, as in `filmcraft-mpeg2v`, within IEEE 1180.
- Damage: a macroblock that does not parse ends its packet; decoding resumes at the next resync
  marker, and what no packet reached comes from the reference. Such VOPs still show, and are
  counted (`Decoder::errors`, `last_error`). `decode` returns an error when nothing can be shown:
  no layer header, or a feature the decoder lacks. Nothing panics on truncated or mutated input
  (`damaged_streams_never_panic`, here and in `codecs`).

### Interpretations

Where the implementations in use settle what the text leaves open, the tests against FFmpeg agree:

- Intra DC with the AC table: the "running QP" of `intra_dc_vlc_thr` is the quantiser before this
  macroblock's dquant.
- Direct mode predicts four 8x8 blocks. Only with quarter samples does that differ from one 16x16
  block, as the 8-tap filter mirrors at block ends. Early Xvid builds used 16x16 there.
- Macroblocks of a B-VOP that the next anchor did not code take no bits, so a video packet can
  begin before or after them and can hold nothing else.
- The direct vector delta wraps to its range like any vector with `fcode` 1.

### Not supported

Each is an `Unsupported` error that names the feature, never wrong pictures: interlaced coding, GMC
and static sprites (S-VOPs), arbitrary shape, reversible VLCs, more than 8 bits, overlapped motion
compensation, reduced resolution VOPs, NEWPRED, scalability, complexity estimation headers.

Short video header (H.263) streams have no layer header and are refused. MS-MPEG4 (DIV3, MP43,
MP42) is another bitstream and not decoded here. The bugs of old encoders are not emulated (the
quarter-sample chroma of Xvid before 1.0).

## Accuracy (vs FFmpeg as an external oracle)

The standard leaves the IDCT's rounding open, so frames are compared by PSNR: **every frame at least
45 dB**. Frame count and display order are exact. `tests/oracle.rs`, FFmpeg's encoder (it stays
within ±6, and does not drift: frame 300 of a single GOP is as close as frame 30):

| Fixture | Content | Frames | Min dB | Mean dB | Max Δ |
|---|---|---:|---:|---:|---:|
| sp_default | 352×288, q 4, one GOP | 300 | 57.40 | 59.73 | 6 |
| sp_gop12 | 1 Mb/s, GOP 12 | 120 | 58.00 | 61.67 | 3 |
| sp_q1 | q 1 | 60 | 58.76 | 61.19 | 3 |
| sp_q2_escapes | strong noise, q 2 | 30 | 62.56 | 63.79 | 3 |
| sp_q31 | q 31 | 120 | 52.86 | 54.05 | 2 |
| sp_mbd2_trellis | `-mbd 2 -trellis 1` | 120 | 62.05 | 62.86 | 3 |
| sp_4mv | four vectors | 120 | 64.84 | 65.85 | 2 |
| sp_acpred | AC prediction, GOP 5 | 60 | 53.48 | 53.70 | 2 |
| sp_mpeg_quant | MPEG quantisation | 120 | 61.64 | 62.25 | 3 |
| sp_custom_matrix | custom matrices | 60 | 59.14 | 59.81 | 3 |
| sp_packets | video packets | 120 | 61.99 | 64.16 | 3 |
| sp_partitioned | data partitioning | 120 | 61.99 | 64.16 | 3 |
| sp_partitioned_4mv | data partitioning, four vectors | 60 | 53.44 | 53.62 | 2 |
| sp_odd_size | 322×243 | 60 | 63.33 | 65.09 | 3 |
| sp_fast_motion | large vectors | 90 | 68.61 | 70.42 | 2 |
| asp_b1 | one B-VOP | 150 | 62.89 | 64.54 | 3 |
| asp_b2 | two B-VOPs, one GOP | 300 | 62.02 | 63.66 | 4 |
| asp_b2_direct_rich | B-VOPs, four vectors | 150 | 62.20 | 63.22 | 2 |
| asp_b2_mpeg_quant | B-VOPs, MPEG quantisation | 90 | 62.03 | 62.57 | 2 |
| asp_b2_packets | B-VOPs in packets | 90 | 65.71 | 66.23 | 2 |
| asp_b2_partitioned | partitioned, B-VOPs whole | 90 | 65.71 | 66.23 | 2 |
| asp_qpel | quarter sample | 150 | 60.54 | 63.41 | 4 |
| asp_qpel_b2_4mv | quarter sample, B-VOPs, four vectors | 150 | 59.65 | 62.60 | 4 |

FFmpeg's encoder uses a small part of the syntax, so `src/synth_tests.rs` writes random valid
streams from the syntax alone: all 204 coefficient table entries, the three escapes, DC in the AC
table, macroblock stuffing, header extension codes, direct deltas, wide vectors far outside the
frame. FFmpeg's decode stays within ±3 on all of them (12 configurations, 5 seeds each).

One difference is left out of that test: four vectors more than 16 samples past the right or bottom
edge of a picture whose size is not a multiple of 16. There FFmpeg stops the block at the picture
size; this decoder clamps at the macroblock-aligned frame, as it does everywhere else. Real encoders
do not reach there.

`crates/codecs/tests/it/mpeg4_oracle.rs` has the same comparison through AVI: B-VOPs in stream order,
the packed bitstream (written by the test; FFmpeg reads the same pictures from the file), the layer
header only in `strf`, no index, open GOPs, and random seeks equal to sequential decoding.

The FFmpeg used here has no libxvid, so the committed tests use its `mpeg4` encoder. `real_file` in
`mpeg4_oracle.rs` (ignored; `MPEG4_SAMPLE=<file>`) compares any AVI from several seek points. Two
XVID-tagged files from the field were compared by hand: a Simple Profile one over 299 frames (min
65.9 dB) and an Advanced Simple one with packed B-VOPs over 1,200 frames from four seek points (min
52.8 dB). In the packed file FFmpeg's seek lands one frame later than this decoder's slot, so it
was compared with that offset; each chunk's slot is taken as its display position here.

## Speed

`cargo test --release -p filmcraft-mpeg4v --test perf -- --ignored --nocapture`, one thread, Apple
M-series, generated 720×404 video at 1.5 Mb/s, decode and crop to planes:

| Stream | fps |
|---|---:|
| Simple Profile | 3840 |
| Simple Profile, all moving | 3960 |
| B-VOPs | 3460 |
| B-VOPs, quarter sample | 1560 |

The clips are generated; film has more residual and more varied vectors, so take these as an upper
bound. Going through `AviSource` and its frame cache costs a little over 2000 fps on the same size.
