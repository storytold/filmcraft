# filmcraft-alac

A clean-room ALAC (Apple Lossless Audio Codec) encoder, written from the published format. Layer
L0: it depends only on `filmcraft-bitstream` and `thiserror`, has no `unsafe`, and builds for
`wasm32-unknown-unknown`. FilmCraft decodes ALAC through symphonia; this crate is the export side.

## Usage

```rust
use filmcraft_alac::{Encoder, EncoderConfig};

let mut enc = Encoder::new(EncoderConfig::new(48_000, 2, 24))?; // rate, channels (1–8), bits (16/20/24)
let mut frames = enc.encode_frames(&[&left, &right])?;          // planar i32, ALAC channel order → whole frames
frames.extend(enc.finish());                                     // the last, shorter frame
let cookie = enc.magic_cookie();                                 // 24-byte ALACSpecificConfig for the `alac` box
```

One frame is one MP4 / QuickTime sample (4096 samples per channel by default; the last one is
shorter and says so in its header). The cookie's largest-frame and average-bitrate fields are
filled in as frames are encoded, so take it again at the end.

## What is written

| Part | Implemented |
|---|---|
| Elements | single channel (SCE) and channel pair (CPE) elements in ALAC's channel maps for 1–8 channels (5.1 = C L R Ls Rs LFE), then END; partial last frame with its sample count |
| Prediction | ALAC's adaptive filter, 8 taps, denominator shift 9, mode 0; the coefficients the decoder ends a frame with start the next one (Apple's starting values in the first frame and after a verbatim one) |
| Stereo | left/right, mid/side and left/side mixing (mixBits 2, mixRes 0 / 2 / 4), the smallest kept per frame |
| Residual | ALAC's adaptive Golomb code (pb 40, mb 10, kb 14) with zero runs and escapes |
| High bit depths | 24-bit samples send their low 8 bits verbatim (`bytesShifted` 1), so the predicted part is at most 16 bits plus the pair's extra bit. No 32-bit: decoders handle it inconsistently (ffmpeg cannot decode a verbatim 32-bit channel pair), so `Encoder::new` refuses it |
| Fallback | an element that would not shrink is stored verbatim |

Decoders disagree in two corner cases of the residual code: a value of exactly 2^16 coded right
after a zero run (ffmpeg resets the running mean, Apple's decoder does not), and zero runs of
65535 samples or more. The encoder never writes either: frames are at most 16384 samples, and a
channel that would meet the first falls back to another mix or to verbatim storage.

## Verification

`cargo test -p filmcraft-alac`: a reference decoder of whole frames (Apple's rules) round-trips
every depth (16/20/24), mono, stereo and 5.1, frame lengths 16–16384 and partial frames; the
predictor's reconstruction matches the encoder's coefficient adaptation sample for sample, also
where the 32-bit sums wrap.

`tests/ffmpeg_oracle.rs` (ffmpeg as an external decoder, skipped when absent) wraps the frames in a
CAF file and requires ffmpeg to decode every stream without a message to exactly the samples that
went in: music-like tones, a sweep, white noise, a full-scale square, silence and a click at 16,
20 and 24 bits; mono, 3 channels and 5.1; 1152-, 4096- and 16384-sample frames. On
music-like stereo the stream is smaller than ffmpeg's own ALAC encoder makes it (204 KB against
277 KB for two seconds). FilmCraft's own import (symphonia) is checked in the export tests.
