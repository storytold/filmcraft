# filmcraft-mp3enc

Clean-room MPEG-1 / MPEG-2 Audio Layer III (MP3) encoder in pure Rust. Layer L0: depends only on
`filmcraft-bitstream` and `thiserror`. Builds for `wasm32-unknown-unknown`; no `unsafe`. No
GPL/LGPL code (LAME, shine, FFmpeg, libmp3lame) was consulted; FFmpeg is used only as an external
test oracle.

## Specification

ISO/IEC 11172-3:1993 "Coding of moving pictures and associated audio for digital storage media at
up to about 1,5 Mbit/s — Part 3: Audio": §2.4.1 bit stream syntax (header, `audio_data()`,
`side_info`, `main_data`, `huffmancodebits`), §2.4.2 semantics (bit rates, scalefactor selection,
`scalefac_compress`), §2.4.3.4 the Layer III decoding process (requantization, alias butterflies,
IMDCT and frequency inversion, mirrored here), Annex B tables (3-B.3 window, 3-B.7 Huffman
codes, 3-B.8 scalefactor bands, 3-B.9 alias coefficients) and Annex C (encoding process: C.1.3
analysis subband filter with the window `C[i] = D[i] / 32` of Table 3-C.1, C.1.5.4 the
quantization loops). ISO/IEC 13818-3:1998 §2.4 for the lower sampling frequencies (one granule per
frame, 9-bit `scalefac_compress`, the LSF bit rates and scalefactor bands). The threshold in quiet
is Terhardt's approximation, as in the 11172-3 Annex D models.

## API

```rust
let mut enc = filmcraft_mp3enc::Encoder::new(filmcraft_mp3enc::Config::cbr(48_000, 2, 192))?;
let mut mp3 = enc.encode(&[&left, &right]); // whole frames as they complete
mp3.extend(enc.flush());                     // pads with silence to the last input sample
// a decoder's output lags the input by filmcraft_mp3enc::DELAY (1057) samples
```

`bitrates(rate)`, `nearest_bitrate(rate, kbps)` and `nearest_sample_rate(rate)` map arbitrary
settings onto what a stream can carry.

## Encoder

- Constant bit rate: 32-320 kbit/s at 32 / 44.1 / 48 kHz (MPEG-1, 1152 samples per frame) and
  8-160 kbit/s at 16 / 22.05 / 24 kHz (MPEG-2 LSF, 576 samples per frame); padding slots keep
  44.1 / 22.05 kHz streams at the exact rate.
- Mono, stereo, and joint stereo with per-frame mid/side (when the side signal carries under a
  fifth of the mid energy).
- Hybrid filterbank: the Annex C polyphase analysis filter, the normal-window MDCT (scaled as the
  exact inverse of the decoder's IMDCT) and alias reduction; perfect reconstruction is checked
  against a reference of the normative decoder filterbank to 5·10⁻⁵.
- Psychoacoustic model: band energies spread 10 dB per scalefactor band upwards and 25 dB
  downwards, 12 dB under the masker, floored by the threshold in quiet (full scale = 96 dB SPL);
  a low-pass by bit rate.
- Quantization: inner loop (the finest global gain that fits the budget), outer loop (amplify
  the bands over their threshold, switching to `scalefac_scale` when the scalefactors overflow,
  keep the lowest worst noise-to-mask ratio), then coarsening while every band stays masked so
  that easy granules save bits; exhaustive choice of the region split and of the Huffman table
  per region (all 30 pair tables with the smallest sufficient `linbits`, count1 table A or B).
- Bit reservoir through `main_data_begin`, bounded so that reservoir and frame fit the decoder's
  7680-bit input buffer (so none at 320 kbit/s, 44.1 kHz) and by 511 / 255 bytes; surplus beyond
  it is stuffed.

## Not yet

Block switching (short and mixed blocks; transients pre-echo), scalefactor reuse (`scfsi`) and
`preflag`, intensity stereo, a tonality-aware model, VBR / ABR, the Xing / Info and LAME gapless
tags (the delay is not signalled), CRC, free format.

## Tests

`cargo test -p filmcraft-mp3enc`: filterbank reconstruction and scale, frame headers and sizes,
the reservoir limits, hostile settings and samples, chunking invariance. With ffmpeg available:
every code of every Huffman table (and large escapes) decoded by ffmpeg against a reference
decode of the intended lines, and full encodes at every sampling frequency checked with ffprobe
(codec, rate, channels, duration) and by SNR of ffmpeg's decode against the input.
