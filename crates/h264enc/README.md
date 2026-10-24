# filmcraft-h264enc

A clean-room, pure-Rust H.264/AVC encoder for FilmCraft's export path, written from the public
ITU-T H.264 specification (04/2013 edition) and general encoder literature. Layer L0: it depends only on
`filmcraft-bitstream`, `thiserror` and (optionally, feature `threads`, on by default) `rayon`. No `unsafe`,
builds for `wasm32-unknown-unknown` with or without `threads`.

## Usage

```rust
use filmcraft_h264enc::{Encoder, EncoderConfig, PacketFormat, Preset, Profile, RateControl, YuvFrame};

let mut cfg = EncoderConfig::new(1920, 1080, 30000, 1001);
cfg.profile = Profile::High;
cfg.preset = Preset::Balanced;              // Speed / Balanced / Quality
cfg.rate = RateControl::Crf(20.0);          // Crf(f32) | Cbr { kbps } | Vbr { target_kbps, max_kbps } | Qp(u8)
cfg.bframes = 2;                            // 0..=3
cfg.keyint = 250;
cfg.format = PacketFormat::LengthPrefixed;  // MP4 samples; or AnnexB (default) for .h264
let mut enc = Encoder::new(cfg)?;
let avcc = enc.avcc();                      // AVCDecoderConfigurationRecord for the MP4 `avcC` box
for (i, pic) in pictures.iter().enumerate() {
    let frame = YuvFrame { y: &pic.y, u: &pic.u, v: &pic.v, y_stride: w, uv_stride: w / 2 };
    for p in enc.encode(&frame, i as i64)? { /* p.data, p.pts, p.dts, p.keyframe, p.frame_type */ }
}
for p in enc.flush()? { /* ... */ }
```

- Input: planar 4:2:0 8-bit (`YuvFrame` with strides); any even size, non-multiple-of-16 sizes are
  cropped via the SPS frame cropping. `try_encode` returns an error instead of panicking on short planes.
- Output: one `Packet` per picture in decoding order with `pts` (as given), `dts` (reorder-delayed, always
  `<= pts` and strictly increasing), `keyframe` (IDR) and the frame type/QP. Annex-B packets carry an AUD,
  SPS/PPS on every IDR and an identification SEI on the first picture. With constant QP or CRF the packets
  may come out later than the picture that made them (see [Threading](#threading)); `flush` returns all of
  them, and `packets_due()` says how many a serial encode would have returned by now.
- Two-pass: encode once with `cfg.pass = Pass::First`, take `enc.pass_stats()` (serialisable with
  `PassStats::to_text/from_text`), then encode again with `Pass::Second(stats)`.
- `set_recon_capture(true)` + `take_recon()` expose the encoder's reconstructed pictures (used by the tests to
  prove encoder/decoder match).

## Features

| Area | Implemented |
|---|---|
| Profiles | Constrained Baseline (CAVLC, I/P), Main (CABAC, I/P/B), High (CABAC, 8x8 transform, I/P/B); 4:2:0 8-bit, frame coding |
| Parameter sets | SPS (level auto-selected from size/rate/DPB/bitrate, cropping, VUI: SAR, colour description + range, timing, bitstream restriction/reorder), PPS, AUD, user-data SEI, avcC |
| Intra | 16x16 (4 modes, SATD), 4x4 (9 modes, SATD + mode-cost, sequential reconstruction), 8x8 (9 modes with reference filtering, High), chroma (4 modes) |
| Inter P | hexagon integer search from predictors, half- then quarter-sample refinement (SATD) on precomputed 6-tap half-sample planes; 16x16/16x8/8x16/8x8 partitions (Balanced/Quality); P_Skip detection with the normative skip MV |
| Inter B | 1–3 non-reference B-frames, spatial direct / B_Skip, L0/L1/Bi 16x16 and (Balanced/Quality) 16x8/8x16 with a per-partition direction; all B-frames of a mini-GOP are encoded concurrently |
| Residual | 4x4 / 8x8 integer transforms, Intra16x16 and chroma DC Hadamards, deadzone quantisation (intra 1/3, inter 1/6), adaptive 4x4/8x8 inter transform choice, coefficient decimation for inter blocks |
| Entropy | CABAC (all I and cabac_init_idc 0 context tables, verified against the spec text) and CAVLC (all coeff_token/total_zeros/run_before tables, level escapes) |
| Loop filter | normative deblocking (bS 0–4 incl. bi-predicted rules, 8x8-transform edges, chroma QP mapping) in the reconstruction loop |
| Mode decision | SATD + λ·bits for Speed/Balanced; Quality adds RD-lite (SSD + λ·CABAC-estimated bits) between the best intra/inter/skip candidates |
| Rate control | `Qp`, `Crf` (constant quality with I/P/B offsets), 1-pass ABR with complexity model + VBV (`Cbr`, 1-pass `Vbr`), closed-loop 2-pass VBR; variance adaptive quantisation (`aq_strength`), lookahead scene-cut IDR insertion |
| Threading | slice-parallel encoding (MB-row slices, `disable_deblocking_filter_idc = 2` with >1 slice so each slice is also deblocked in parallel), parallel half-sample interpolation and lookahead, the B-frames of a mini-GOP coded together, and for constant QP / CRF whole GOPs coded concurrently (same bytes, see [Threading](#threading)) |

## Threading

With the `threads` feature (on by default) the encoder has its own rayon pool of `threads` threads
(0 = every core) and uses it at three levels:

- **Slices.** A picture is cut into MB-row slices (`slices`, or one per thread up to one per four MB rows),
  coded and deblocked in parallel. The slice count shapes the stream: an export fixes it from the picture
  height so the file does not depend on the machine.
- **B-frames.** The B pictures of a mini-GOP reference only the two anchors around them, so they are coded
  together (all their slices at once) after the P picture.
- **GOPs** (constant QP and CRF only). `try_encode` keeps its serial input side on the calling thread:
  import, half-resolution analysis against the previous source picture, AQ offsets, the IDR / scene-cut
  decision, the mini-GOP structure, each picture's place in coding order and its decoding time. Coding
  moves to one thread per GOP, started at the GOP's IDR with its own reference state; the GOP's batches
  (IDR, then each P picture and its B pictures, then whatever the next IDR or a flush finishes) reach that
  thread as they are formed and are coded in that order by the same code as the serial path. When the
  input comes faster than a GOP can be coded, the next GOPs start while the earlier ones finish, so several
  GOPs run at once, their slices sharing the pool. A small pool of its own analyses the input meanwhile
  (on the shared pool it waited behind the slices and starved the GOPs).

  The bytes are the serial path's: IDRs are closed (an IDR empties the DPB and restarts `frame_num` and the
  POC), and the few counters a GOP takes from the stream (the `idr_pic_id` parity, the reference picture
  numbering, the identification / HDR SEI on the first picture only) are given to its thread. Constant QP
  and CRF choose a picture's QP from its type alone. Packets, rate-control statistics, first-pass
  statistics and reconstructions are taken back in decoding order, so a packet only comes out once every
  packet before it is coded: later than on the serial path. `packets_due()` is the number of packets the
  serial path would have returned by now; the export interleaves its audio by it, so a CRF export is the same
  file either way (`crates/export/src/determinism_tests.rs`).

  Memory: the input pictures not coded yet plus each GOP thread's working set (references with their
  half-sample planes, reconstructions) stay within 1 GiB of padded pictures; `try_encode` waits for the
  oldest GOPs when the next picture, or the working set of a GOP about to start, would pass it. When two
  GOPs of `keyint` pictures do not fit (1080p with `keyint` 250, 4K with a 2-second `keyint`), or with one
  thread, the encoder stays serial (so with the older GOPs taken a new one always fits). A GOP
  thread that panics turns into `Error::Worker`, returned by the first `try_encode` / `flush` after its
  death is noticed: that GOP's remaining packets are lost, it takes no more pictures, and when the stream
  would have continued from it the next picture is coded as an IDR, so no later picture refers to a lost
  one; the other GOPs and later calls carry on. Dropping the encoder stops every GOP thread after its
  current batch and waits for it, so the GOPs' pictures, buffers and pool work are released by the time
  the drop returns.
  `EncoderConfig::parallel_gops = false` forces the serial path; `gop_parallel()` says which path an encoder
  took. Measurements: `docs/performance.md`, "H.264: CRF and constant-QP GOPs in parallel".

  The bitrate modes (CBR, one-pass VBR, both passes of two-pass VBR) stay serial: each picture's QP comes
  from the actual size of the pictures coded before it (ABR model, VBV fill, two-pass model error), and
  per-macroblock AQ is applied to that fractional frame QP, so no GOP can be coded before the previous one
  without changing the bytes.

## Verification

`cargo test --release -p filmcraft-h264enc` (ffmpeg/ffprobe used only as external oracles; tests skip when
absent). Every stream is decoded with `ffmpeg -v warning -err_detect +crccheck+bitstream+buffer+explode -f rawvideo`
and must print nothing (not even a `corrupt decoded frame` warning), produce exactly the number of frames encoded,
and be **bit-identical to the encoder's reconstruction** — this covers Baseline/Main/High, all presets, 0–3
B-frames, 1–4 slices and automatic slicing at 1080p, odd sizes, `testsrc2` and `mandelbrot` sources, and
length-prefixed output re-wrapped with the avcC parameter sets. B-frame display order is checked with ffprobe. A
negative control drops and truncates slices and checks that the oracle reports them.

The oracle leaves ffmpeg's error concealment on. With `-ec 0`, ffmpeg 7.1 and 8.1 print `corrupt decoded frame`
for every picture that has more than one slice, libx264's multi-slice streams included, while the decoded pixels
are unchanged (#74). The bit-exact comparison is what rules out concealment.

Results on the synthetic content (moving gradient, grain noise, text-like edges, sub-pixel panning):

| Test | Result |
|---|---|
| 1080p, High, Speed, CRF 20, 2 B | mean luma PSNR 43.0 dB (IDR 39.1, P/B 43.2–44.5) |
| 640x360 CBR 1500 kbit/s | 1518 kbit/s (+1.2%) |
| 640x360 1-pass VBR 1000 kbit/s | 981 kbit/s (−1.9%) |
| 640x360 2-pass VBR 800 kbit/s | 840 kbit/s (+5.0%) |

Development tool: `cargo run --release -p filmcraft-h264enc --example h264enc_synth -- W H FRAMES
[profile] [preset] [bframes] [qpN|crfN|cbrN|vbrN] [slices] [synth|testsrc2|mandelbrot]` encodes, decodes with
ffmpeg and reports speed (wall and CPU time), bitrate, PSNR and any encoder/decoder mismatch (`MAP=1` prints a
per-MB mismatch map, `PERF=k` cycles k source pictures for speed runs, `GOPS=0` forces the serial path,
`THREADS` / `KEYINT` / `AQ` override the settings).

`tests/it/gop_parallel.rs` encodes CRF and constant-QP streams both ways and requires identical packets
(bytes, pts, dts, keyframe, type, QP) and reconstructions over 0–3 B-frames, Baseline / Main / High, 1 and
4 slices (and 15 at 320x240), `keyint` 5 and 250 with scene cuts inside a `keyint`, Annex B and
length-prefixed output, HDR SEI, flushes in the middle of a mini-GOP and a partial last GOP, reconstruction
capture switched on mid-stream and pools of 2, 3 and 8 threads; also that the bitrate modes, a single thread
and a picture buffer over budget take the serial path, and that CRF first-pass statistics match.

## Limitations / not yet implemented

- Single reference frame per list; no B-pyramid, no weighted prediction, no interlaced/MBAFF, no 4:2:2/4:4:4 or
  high bit depth, no custom scaling matrices, no sub-8x8 partitions, no B_8x8.
- CAVLC is used only for Baseline (High/Main always use CABAC); 8x8 transform is therefore CABAC-only.
- No trellis quantisation, no psy-RD; no HRD/buffering-period SEI (VBV is enforced internally only).
- No frame-level pipelining inside a GOP (a picture starts once its reference is complete); with many slices,
  slice edges are not deblocked.
