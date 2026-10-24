# filmcraft-mpa

Clean-room MPEG-1 / MPEG-2 Audio Layer I and Layer II decoder (MP1 / MP2) in pure Rust. Layer L0:
depends only on `filmcraft-bitstream` and `thiserror`. Builds for `wasm32-unknown-unknown`; no
`unsafe`. No GPL/LGPL code (FFmpeg, libmad, mpg123, twolame) was consulted; FFmpeg (and twolame,
when installed) are used only as external fixture generators and test oracles.

## Why

symphonia (the bootstrap decoder for MP3 and others) rejects every Layer II frame whose bit rate /
channel mode combination ISO/IEC 11172-3 does not allow: mono above 192 kbit/s and stereo below
64 kbit/s. ffmpeg writes such streams by default (mono MP2 is 384 kbit/s unless asked otherwise),
so those tracks played as silence in every container (#809).

## Specification

ISO/IEC 11172-3:1993: §2.4.1 bit stream syntax (header, Layer I and Layer II audio data),
§2.4.2 semantics, §2.4.3.1 frame length, §2.4.3.2 synthesis subband filter (Figure A.2),
§2.4.3.3 Layer I and §2.4.3.4 Layer II decoding; Annex B: Table 3-B.1 scalefactors (computed as
2^(1 - i/3)), Table 3-B.2a-d Layer II allocation tables, Table 3-B.3 synthesis window (the 257
coefficients `D[0..=256]`, exact multiples of 2⁻¹⁶, the rest from the prototype's symmetry;
a unit test checks the typed table is the smooth prototype), Table 3-B.4 quantization classes.
ISO/IEC 13818-3: the lower sampling frequencies (16 / 22.05 / 24 kHz), their bit rate tables and
the Layer II allocation table B.1 (its rows confirmed against FFmpeg's decode).

## API

```rust
let mut dec = filmcraft_mpa::Decoder::new();
let h = filmcraft_mpa::parse_header(frame)?;   // layer, rate, mode, frame size
let out = dec.decode(frame)?;                  // h.samples() per channel (384 or 1152)
dec.reset();                                    // before decoding from another position
```

## Decoder

- Layer I and Layer II; mono, stereo, dual channel and joint (intensity) stereo with every bound;
  MPEG-1, MPEG-2 lower sampling frequencies and the unofficial MPEG-2.5; with or without CRC.
- Layer II table selection by sampling frequency and bit rate per channel, with the ranges of
  Table 3-B.2 taken as open-ended so the disallowed combinations decode the way encoders write
  them (mono 384 kbit/s uses table a at 48 kHz, b at 44.1 / 32 kHz; stereo 32 / 48 kbit/s uses
  c, or d at 32 kHz).
- Requantization `(2v + 1 - steps) / steps` (the standard's `C · (s''' + D)` for every class),
  grouped 3-, 5- and 9-step codewords, scalefactor selection information.
- Output: planar f32 at full scale ±1.
- Errors (forbidden allocation, reserved header fields, data running past the frame) return
  `Err`; garbage never panics (unit fuzz test and damaged-frame oracle test).

## Accuracy (vs FFmpeg as an external oracle)

`tests/oracle.rs`, worst channel SNR against FFmpeg's float decoder. FFmpeg's own fixed-point
decoder agrees with its float decoder to ~77 dB (the error sits at the 16-bit LSB); the test
requires 70 dB, and a wrong table entry or misread field costs tens of dB.

| Stream | SNR |
|---|---|
| Layer II mono 48 kHz, FFmpeg's default 384 kbit/s (not allowed by 11172-3) | 77.1 dB |
| Layer II mono 44.1 kHz 320 kbit/s pink noise (not allowed) | 72.9 dB |
| Layer II stereo 48 kHz 32 kbit/s (table c; not allowed) | 83.2 dB |
| Layer II stereo 48 kHz 192 kbit/s, 44.1 kHz 256 / 96 kbit/s, 32 kHz 128 kbit/s | 83.3-83.6 dB |
| Layer II mono 32 kHz 48 kbit/s (table d) | 76.9 dB |
| Layer II MPEG-2: stereo 24 kHz 160 kbit/s, mono 22.05 kHz 64 kbit/s, stereo 16 kHz 32 kbit/s | 76.9-83.5 dB |
| Layer II joint stereo and dual channel from twolame (when installed) | 83.4-83.6 dB |
| Synthetic valid random frames: Layer I mono / stereo / dual / joint stereo at every bound, MPEG-1 and MPEG-2; Layer II joint stereo at every bound and dual channel with every allocation table | 74.1-80.0 dB |

No installed encoder writes Layer I, so Layer I is checked on synthetic frames (random
allocations, scalefactors and samples) that FFmpeg decodes as it would any stream. Through
`filmcraft-codecs` (`tests/mpeg_oracle.rs`), mono 384 kbit/s MP2 in MPEG-TS, Matroska and a raw
`.mp2` file is within 3.1·10⁻⁵ of FFmpeg's decode.

## Gaps

- Layer III is not decoded here (symphonia does it); `parse_header` reads its headers.
- The free format (bit rate index 0) is refused: its frame length is only known from the next
  sync word.
- The CRC is read, not checked; ancillary data is skipped.
