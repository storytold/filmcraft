# filmcraft-flac

A clean-room FLAC encoder (Free Lossless Audio Codec, [RFC 9639](https://www.rfc-editor.org/rfc/rfc9639)),
written from the RFC. Layer L0: it depends only on `filmcraft-bitstream` and `thiserror`, has no
`unsafe`, and builds for `wasm32-unknown-unknown`.

No libFLAC, FFmpeg or other FLAC implementation was read or ported. ffmpeg is used only as an
external **test oracle** (decoding our streams). Decoding FLAC in FilmCraft is still done by the
symphonia bootstrap decoder in `filmcraft-codecs`.

## API

```rust
use filmcraft_flac::{Encoder, EncoderConfig, STREAMINFO_OFFSET};

let mut cfg = EncoderConfig::new(48_000, 2, 24);   // rate, channels (1–8), bits (8–24)
cfg.level = 8;                                     // 0 fastest … 8 smallest; 5 by default
let mut enc = Encoder::new(cfg)?;
let mut file = enc.header();                       // fLaC + STREAMINFO + VORBIS_COMMENT (vendor)
file.extend(enc.encode(&[&left, &right])?);        // planar i32 samples, any chunk size → whole frames
file.extend(enc.finish());                         // the last, shorter frame
// frame sizes, total samples and MD5 are only known now: patch STREAMINFO (or seek back in a file)
let at = STREAMINFO_OFFSET as usize;
file[at..at + 34].copy_from_slice(&enc.streaminfo());

// MP4 / QuickTime (`fLaC` + `dfLa`): one frame per sample, the STREAMINFO body in `dfLa`
let frames: Vec<Vec<u8>> = enc.encode_frames(&[&left, &right])?;   // each a full block
let last_len = enc.pending_samples();                               // duration of the frame finish() writes
```

## What it writes

| Part | |
|---|---|
| Metadata | STREAMINFO (block size, frame sizes, rate, channels, sample size, total samples, MD5 of the audio) and a VORBIS_COMMENT with the vendor string |
| Frames | fixed block size (4096 by default), frame numbers, sample rate and size codes, CRC-8 header and CRC-16 frame checksums |
| Subframes | constant, verbatim, fixed predictors (order 0–4) and linear prediction (up to order 12), whichever is smallest |
| Linear prediction | Tukey(0.5) windowed autocorrelation, Levinson–Durbin, 12/15-bit quantised coefficients with error feedback |
| Residual | partitioned Rice codes (4- or 5-bit parameters), the partition order chosen per subframe |
| Stereo | independent, left/side, side/right or mid/side per frame, whichever is smallest |

Not written: variable block sizes, wasted-bits shifts, escape-coded partitions, 32-bit samples,
seek tables.

| Level | LPC order | Rice partition order | Stereo decorrelation |
|---|---|---|---|
| 0 | fixed only | ≤ 3 | no |
| 1–2 | fixed only | ≤ 3 | yes |
| 3 | ≤ 6 | ≤ 4 | no |
| 4 | ≤ 8 | ≤ 4 | yes |
| 5 (default) | ≤ 8 | ≤ 5 | yes |
| 6 | ≤ 8 | ≤ 6 | yes |
| 7–8 | ≤ 12 | ≤ 6 | yes |

## Tests

`cargo test -p filmcraft-flac`: CRC check values, the RFC 1321 MD5 suite, frame-number coding,
predictor round trips, and `tests/ffmpeg_oracle.rs`, which decodes every stream with ffmpeg and
requires every sample back exactly, no decoder errors, and the STREAMINFO MD5 equal to ffmpeg's
hash of the decoded audio. It covers music-like, sweep, noise, full-scale square, silence and click
signals; 8/16/20/24-bit; mono, stereo and 5.1; common and odd sample rates; block sizes 16 to 65535;
and every level. Skipped (with a `SKIPPED` line) without ffmpeg.
