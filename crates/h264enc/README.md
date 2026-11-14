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
    for p in enc.encode(&frame, i as i64) { /* p.data, p.pts, p.dts, p.keyframe, p.frame_type */ }
}
for p in enc.flush() { /* ... */ }
```

- Input: planar 4:2:0 8-bit (`YuvFrame` with strides); any even size, non-multiple-of-16 sizes are
  cropped via the SPS frame cropping. `try_encode` returns an error instead of panicking on short planes.
- High 10: `cfg.profile = Profile::High10; cfg.bit_depth = 10;` (8 to 10) and `enc.encode16(&YuvFrame16 { … })`
  with samples in the low bits of each `u16` (values above the bit depth are clipped); 8-bit `YuvFrame`s are
  also taken and scaled up exactly. `cfg.intra_only = true` codes every picture as an IDR without reference
  frames (High 10 Intra with constraint_set3 for High 10).
- Output: one `Packet` per picture in decoding order with `pts` (as given), `dts` (reorder-delayed, always
  `<= pts` and strictly increasing), `keyframe` (IDR) and the frame type/QP. Annex-B packets carry an AUD,
  SPS/PPS on every IDR and an identification SEI on the first picture.
- Two-pass: encode once with `cfg.pass = Pass::First`, take `enc.pass_stats()` (serialisable with
  `PassStats::to_text/from_text`), then encode again with `Pass::Second(stats)`.
- `set_recon_capture(true)` + `take_recon()` expose the encoder's reconstructed pictures (used by the tests to
  prove encoder/decoder match): one byte per sample at 8 bits, two little-endian bytes above (`yuv420p10le`).

## Features

| Area | Implemented |
|---|---|
| Profiles | Constrained Baseline (CAVLC, I/P), Main (CABAC, I/P/B), High (CABAC, 8x8 transform, I/P/B), 4:2:0 8-bit; High 10 and High 10 Intra (4:2:0, 8 to 10 bits); frame coding |
| Bit depth | the encoder is generic over the sample type: `u8` at 8 bits (the same code as before High 10), `u16` above; QP' = QP + QpBdOffset in quantisation and lambdas (QPY down to −6·(BitDepth − 8)), mb_qp_delta range and wrap, chroma QP mapping, α/β/tC0 scaled by 2^(BitDepth − 8), clipping in prediction, interpolation and reconstruction; SATD heuristics scaled; lookahead, AQ and rate control work on 8-bit-scaled statistics |
| Parameter sets | SPS (level auto-selected from size/rate/DPB/bitrate with the profile's cpbBrVclFactor, cropping, bit depth, VUI: SAR, colour description + range, timing, bitstream restriction/reorder), PPS, AUD, user-data SEI, avcC (with the High-profile chroma format and bit depth fields) |
| Intra | 16x16 (4 modes, SATD), 4x4 (9 modes, SATD + mode-cost, sequential reconstruction), 8x8 (9 modes with reference filtering, High), chroma (4 modes) |
| Inter P | hexagon integer search from predictors, half- then quarter-sample refinement (SATD) on precomputed 6-tap half-sample planes; 16x16/16x8/8x16/8x8 partitions (Balanced/Quality); P_Skip detection with the normative skip MV |
| Inter B | 1–3 non-reference B-frames, spatial direct / B_Skip, L0/L1/Bi 16x16 and (Balanced/Quality) 16x8/8x16 with a per-partition direction; all B-frames of a mini-GOP are encoded concurrently |
| Residual | 4x4 / 8x8 integer transforms, Intra16x16 and chroma DC Hadamards, deadzone quantisation (intra 1/3, inter 1/6), adaptive 4x4/8x8 inter transform choice, coefficient decimation for inter blocks |
| Entropy | CABAC (all I and cabac_init_idc 0 context tables, verified against the spec text) and CAVLC (all coeff_token/total_zeros/run_before tables, level escapes) |
| Loop filter | normative deblocking (bS 0–4 incl. bi-predicted rules, 8x8-transform edges, chroma QP mapping) in the reconstruction loop |
| Mode decision | SATD + λ·bits for Speed/Balanced; Quality adds RD-lite (SSD + λ·CABAC-estimated bits) between the best intra/inter/skip candidates |
| Rate control | `Qp`, `Crf` (constant quality with I/P/B offsets), 1-pass ABR with complexity model + VBV (`Cbr`, 1-pass `Vbr`), closed-loop 2-pass VBR; variance adaptive quantisation (`aq_strength`), lookahead scene-cut IDR insertion |
| Threading | slice-parallel encoding (MB-row slices, `disable_deblocking_filter_idc = 2` with >1 slice so each slice is also deblocked in parallel), parallel half-sample interpolation and lookahead |

## Verification

`cargo test --release -p filmcraft-h264enc` (ffmpeg/ffprobe used only as external oracles; tests skip when
absent). Every stream is decoded with `ffmpeg -v warning -err_detect +crccheck+bitstream+buffer+explode -f rawvideo`
and must print nothing (not even a `corrupt decoded frame` warning), produce exactly the number of frames encoded,
and be **bit-identical to the encoder's reconstruction** — this covers Baseline/Main/High, all presets, 0–3
B-frames, 1–4 slices and automatic slicing at 1080p, odd sizes, `testsrc2` and `mandelbrot` sources, and
length-prefixed output re-wrapped with the avcC parameter sets. B-frame display order is checked with ffprobe. A
negative control drops and truncates slices and checks that the oracle reports them.

High 10 (`tests/high10_oracle.rs`) is decoded as `yuv420p10le` (`yuv420p9le` at 9 bits) and must equally be
bit-identical to the reconstruction: all presets, 0–3 B-frames, 1–4 slices, odd sizes, QP 22–46, CRF down to −12
(QP' 0), CBR, intra-only, 8-bit input, High 10 at 8 bits and length-prefixed output with the avcC. ffprobe reports
`High 10` / `High 10 Intra` and `yuv420p10le`. On a smooth 10-bit source, High 10 at CRF −12 measures 75 dB luma
PSNR against 62 dB for an 8-bit High encode at QP 0 (which the 8-bit rounding caps at about 59–62 dB). 8-bit streams
are byte-identical to those of the encoder before High 10.

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
ffmpeg and reports speed, bitrate, PSNR and any encoder/decoder mismatch (`MAP=1` prints a per-MB mismatch map,
`PERF=k` cycles k source pictures for speed runs).

## Limitations / not yet implemented

- Single reference frame per list; no B-pyramid, no weighted prediction, no interlaced/MBAFF, no 4:2:2/4:4:4,
  no bit depths above 10, no lossless (transform bypass), no custom scaling matrices, no sub-8x8 partitions, no
  B_8x8 (#1221 plans High 4:2:2, High 4:4:4 Predictive and lossless).
- CAVLC is used only for Baseline (High/Main always use CABAC); 8x8 transform is therefore CABAC-only.
- No trellis quantisation, no psy-RD; no HRD/buffering-period SEI (VBV is enforced internally only).
- Threading is slice-based only (no frame-level pipelining); with many slices, slice edges are not deblocked.
