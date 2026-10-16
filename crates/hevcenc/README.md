# filmcraft-hevcenc

A clean-room, pure-Rust H.265/HEVC software encoder for FilmCraft's export path, written from the
public ITU-T Rec. H.265 specification (v8, 08/2021; ISO/IEC 23008-2) and general encoder
literature. Layer L0: it depends only on `filmcraft-bitstream`, `thiserror` and (optionally,
feature `threads`, on by default) `rayon`. No `unsafe`, builds for `wasm32-unknown-unknown`.

## Usage

```rust
use filmcraft_hevcenc::{Encoder, EncoderConfig, PacketFormat, Profile, RateControl, YuvFrame};

let mut cfg = EncoderConfig::new(1920, 1080, 30000, 1001);
cfg.profile = Profile::Main;
cfg.rate = RateControl::Crf(22.0);            // Crf(f32) | Cbr { kbps } | Vbr { target_kbps, max_kbps } | Qp(u8)
cfg.keyint = 250;
cfg.format = PacketFormat::LengthPrefixed;    // MP4 samples; or AnnexB (default)
let mut enc = Encoder::new(cfg)?;
let hvcc = enc.hvcc();                        // HEVCDecoderConfigurationRecord for the MP4 `hvcC` box
for (i, pic) in pictures.iter().enumerate() {
    let frame = YuvFrame { y: &pic.y, u: &pic.u, v: &pic.v, y_stride: w, uv_stride: w / 2 };
    for p in enc.encode(&frame, i as i64) { /* p.data, p.pts, p.dts, p.keyframe, p.frame_type */ }
}
for p in enc.flush() { /* ... */ }
```

- Input: planar 4:2:0 8-bit (`YuvFrame` with strides); any even size, non-multiple-of-CTU sizes are
  cropped via the SPS conformance window. `try_encode` returns an error instead of panicking on
  short planes.
- Output: one `Packet` per picture in coding order with `pts` (as given), `dts` (phase 1 has no
  B-frames, so always equal to `pts`), `keyframe` (IDR) and the frame type/QP. Annex-B packets carry
  an AUD, VPS/SPS/PPS on every IDR and an identification SEI on the first picture.
- `set_recon_capture(true)` + `take_recon()` expose the encoder's reconstructed pictures (used by the
  tests to prove encoder/decoder match).

## Phase 1 scope (this PR)

| Area | Implemented |
|---|---|
| Profile | Main (CABAC, I/P); 4:2:0 8-bit, frame coding, single slice |
| Parameter sets | VPS, SPS (level auto-selected from size/rate, conformance window, VUI: SAR, colour description, timing), PPS, AUD, user-data SEI, `hvcC` |
| Intra | 35 modes (planar, DC, 33 angular) with SATD shortlist, 8x8–32x32 CUs |
| Inter P | integer motion search with half-sample refinement (SATD), P_Skip with the normative merge candidate |
| Residual | 4x4–32x32 integer transforms, 4x4 DST for luma intra, deadzone quantisation (intra 1/3, inter 1/6) |
| Entropy | CABAC (all Main I/P syntax elements, initType 0/1) |
| Loop filter | normative deblocking in the reconstruction loop (SAO off) |
| Mode decision | SATD + λ·bits |
| Rate control | `Qp`, `Crf` (constant quality with an I/P offset), 1-pass ABR with VBV (`Cbr`, 1-pass `Vbr`) |

## Verification

`cargo test --release -p filmcraft-hevcenc` (ffmpeg/ffprobe used only as external oracles; tests skip
when absent). Every stream is decoded with `ffmpeg -v warning -err_detect +crccheck+bitstream+buffer+explode`
and must print nothing, produce exactly the number of frames encoded, and be **bit-identical to the
encoder's reconstruction** — covering I/P mixes, odd sizes and length-prefixed output re-wrapped with
the `hvcC` parameter sets. Display order is checked with ffprobe.

Results are reported here once the slice encoder lands.

## Limitations / not yet implemented (follow-ups)

- No B-frames (`bframes` must be 0), single reference picture, no weighted prediction.
- No SAO, no tiles/WPP, no PCM, no transform skip, no scaling lists, no Main 10.
- No trellis quantisation; no HRD SEI (VBV is enforced internally only); no 2-pass VBR yet.
- Fixed SPLIT-free CU choice per preset is adaptive only by variance (full RDO quad-tree later).
- Threading is lookahead/analysis only (no slice- or frame-level pipelining yet).
